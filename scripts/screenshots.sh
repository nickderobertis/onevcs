#!/usr/bin/env bash
# Capture the terminal screenshots screencomp gates, galleries, and posts to pull
# requests (see screencomp.toml + .github/workflows/visual-docs.yml).
#
# scripts/screenshots-capture.sh drives the REAL release `onevcs` binary against a
# scratch host built the way crates/onevcs/tests/e2e/world.rs builds one — real bare
# origins, real clones, real hooks, real pushes, and that tier's own `gh` stand-in for
# the remote host's decisioning — and normalizes every per-run value away. This script
# renders each scene it wrote to a deterministic SVG with `freeze`, using the VENDORED,
# pinned font (screenshots/fonts/JetBrainsMono-Regular.ttf) embedded into the file as
# base64. So the bytes — and therefore screencomp's digests — are identical on every
# machine and runner without a pinned container. That byte-determinism is the whole
# contract: change a command's output or its formatting and that scene's SVG changes;
# otherwise it does not.
#
# Scenes — see screenshots/AGENTS.md for what each documents and why:
#   help             the whole command surface, styled the way clap styles it
#   rules-check      which policy one repository publishes under, and which rule said so
#   audit-gates      every identity's policy, required checks, and merge-path coverage
#   status           what became of one piece of work, in all five of its sections
#   recoverable      every preserved branch, what stopped it, and the verb that lands it
#   pool-status      the warm worktree slots an identity keeps, and their maintenance
#   release-targets  what a repository releases and how each target is answered
#
# Output (screencomp's capture contract):
#   $SHOTS_OUT/captures.json   index: {schema, shots:[{name,toggles,hash,image}]}
#   $SHOTS_OUT/<scene>.svg     one SVG per scene
# $SHOTS_OUT defaults to shots/current/<arch> (the reusable workflow exports it per
# lane). The SVGs are also copied to docs/screenshots/ (committed) for the README.
#
# Requires `freeze` on PATH (install the pinned version with `just screenshots-tools`).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# This host's capture lane (shots/current/<arch>), named the same way the pre-push
# guard and `just screenshots-bless` name it. CI overrides SHOTS_OUT per lane.
arch="$(bash "$REPO_ROOT/scripts/host-arch.sh")"
SHOTS_OUT="${SHOTS_OUT:-shots/current/$arch}"
if [ -e "$SHOTS_OUT" ] && [ ! -d "$SHOTS_OUT" ]; then
  echo "screenshots: SHOTS_OUT must name a directory to capture into;" >&2
  echo "             $SHOTS_OUT is not one. Unset it or point it elsewhere." >&2
  exit 1
fi
font="$REPO_ROOT/screenshots/fonts/JetBrainsMono-Regular.ttf"
docs_dir="$REPO_ROOT/docs/screenshots"

if ! command -v freeze >/dev/null 2>&1; then
  echo "screenshots: 'freeze' not on PATH. Install the pinned version with:" >&2
  echo "             just screenshots-tools" >&2
  exit 1
fi

# Driving the binary — building it, pointing it at a scratch state root, and
# normalizing what it prints — all belongs to the capture driver. This script renders
# what that driver wrote and nothing else.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bash "$REPO_ROOT/scripts/screenshots-capture.sh" "$work/capture"

# Portable SHA-256 (Linux coreutils vs macOS/BSD).
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Deterministic freeze flags. The vendored font (embedded into the SVG as base64) is
# what makes the output reproducible across machines; everything else is fixed window
# styling. Auto height follows the content, so it only moves when the captured text
# does.
freeze_flags=(
  # Force terminal/ANSI mode. freeze's content-based auto-detection intermittently
  # misreads plain report text as a source file and then ignores --font.file and
  # hangs fetching a default font over the network. `--language ansi` is
  # unconditional, offline, and byte-identical to the auto-detected render: it keeps
  # the ANSI styling clap puts on `--help` and renders the plain scenes verbatim.
  --language ansi
  --font.file "$font"
  --font.family "JetBrains Mono"
  --font.size 14
  --window
  --background "#0d1117"
  --padding "20,30"
  --margin 0
  --border.radius 8
  # Fixed window width + line wrap, so EVERY scene renders at the SAME pixel width.
  # The README and the gallery display each SVG at one fixed width, so a per-scene
  # auto-width would render a narrow scene's text huge and a wide one's tiny. 96
  # columns clears `audit-gates` (110 columns at its widest, folded once) without
  # shrinking the type; 868px = 30+30 padding + 96*8.42px/char at font size 14.
  --width 868
  --wrap 96
)

rm -rf "$SHOTS_OUT"
mkdir -p "$SHOTS_OUT" "$docs_dir"

# captures.json identity is `name + JSON.stringify(toggles)`; entries collect one
# "name|toggles|hash|image" record per rendered scene, sorted at the end. No scene
# here carries a toggle: every one is a different verb rather than one surface at
# different settings, so screencomp.toml declares no `[[toggle]]` either.
entries=()

for source in "$work/capture/scenes"/*.txt; do
  name="$(basename "$source" .txt)"
  image="$name.svg"
  # `< /dev/null`: freeze reads stdin whenever it is not a character device (its
  # IsPipe check), so under CI's piped stdin it would ignore the file argument and
  # render empty input. Pointing stdin at /dev/null forces the read-the-file path.
  freeze "$source" "${freeze_flags[@]}" -o "$SHOTS_OUT/$image" </dev/null >&2
  entries+=("$name|{}|$(sha256 "$SHOTS_OUT/$image")|$image")
  # The committed copies: same bytes, just outside the gitignored shots/ tree.
  cp "$SHOTS_OUT/$image" "$docs_dir/$image"
done

if [ "${#entries[@]}" -eq 0 ]; then
  echo "screenshots: the capture produced no scenes to render" >&2
  exit 1
fi

# Write captures.json, shots sorted by identity, schema 1, trailing newline — the
# exact shape screencomp's classify/manifest/gallery read. Every field is safe ASCII
# (scene names, hex digests, file names), so plain printf is sound.
{
  printf '{\n  "schema": 1,\n  "shots": [\n'
  IFS=$'\n' sorted=($(printf '%s\n' "${entries[@]}" | sort))
  unset IFS
  last=$((${#sorted[@]} - 1))
  for i in "${!sorted[@]}"; do
    IFS='|' read -r name toggles hash image <<<"${sorted[$i]}"
    comma=","
    [ "$i" -eq "$last" ] && comma=""
    printf '    {\n      "name": "%s",\n      "toggles": %s,\n      "hash": "%s",\n      "image": "%s"\n    }%s\n' \
      "$name" "$toggles" "$hash" "$image" "$comma"
  done
  printf '  ]\n}\n'
} >"$SHOTS_OUT/captures.json"

echo "screenshots: wrote ${#entries[@]} shots to $SHOTS_OUT and docs/screenshots/" >&2
