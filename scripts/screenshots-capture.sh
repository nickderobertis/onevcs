#!/usr/bin/env bash
# Drive the REAL `onevcs` binary against a scratch host and write the normalized
# text of every scene the README shows, plus the transcript the animated hero is
# rendered from.
#
# This is the half that knows what the pictures are *of*. Rendering is somebody
# else's job: scripts/screenshots.sh turns these files into SVGs with `freeze`, and
# scripts/demo-gif.py turns the hero transcript into docs/screenshots/demo.gif. One
# driver, so the two renderers can never photograph two different worlds.
#
# Everything here is real except the remote host's decisioning — real bare origins on
# disk, real clones, real executable hooks, real `git push` — which is the recipe
# crates/onevcs/tests/e2e/world.rs draws for the e2e tier. The program installed as
# `gh` IS that tier's own (crates/onevcs/tests/fixtures/gh), so the answers the shots
# are taken against are the answers the journeys are written against. There is no
# model, no network, no credential and no real GitHub anywhere in it.
#
# Usage: scripts/screenshots-capture.sh <work-dir>
#   <work-dir>/scenes/<name>.txt     one still per scene, normalized
#   <work-dir>/hero/steps.tsv        the hero's commands, in order
#   <work-dir>/hero/<step>.txt       each step's output, normalized
#
# `$ONEVCS_BIN` overrides the binary (default: target/release/onevcs).
#
# Unix only, and deliberately: the scenes drive real POSIX hooks and a POSIX `gh`
# stand-in, which is the same gate crates/onevcs/tests/e2e/world.rs states with
# `#![cfg(unix)]` — the repositories this tool drives carry POSIX hooks. The `help`
# scene additionally needs util-linux `script(1)`, because clap styles its help only
# for a terminal and this CLI has no `--color` flag to ask with.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${1:?usage: screenshots-capture.sh <work-dir>}"
BIN="${ONEVCS_BIN:-$REPO_ROOT/target/release/onevcs}"

# The binary the scenes are of: release and locked, the way a user installs it.
# `$SCREENSHOTS_NO_BUILD` skips the build when one is already there, for the inner
# loop; every other caller gets a build.
if [ "$BIN" = "$REPO_ROOT/target/release/onevcs" ] &&
  { [ -z "${SCREENSHOTS_NO_BUILD:-}" ] || [ ! -x "$BIN" ]; }; then
  cargo build --release --locked --bin onevcs >&2
fi
[ -x "$BIN" ] || {
  echo "screenshots-capture: no onevcs binary at $BIN" >&2
  echo "                     build one: cargo build --release --locked --bin onevcs" >&2
  exit 1
}
for tool in git awk script; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "screenshots-capture: '$tool' is not on PATH, and the capture needs it" >&2
    exit 1
  }
done

# Byte-determinism starts with the environment. `onevcs` reads its own `ONEVCS_*`
# settings ahead of everything else, so a shell that exports one steers the scenes
# and prints straight into them; `GH_*`/`GITHUB_*` would reach the substituted host;
# and git's own `GIT_*` would move a commit hash. Clear the lot, then set only what
# this capture itself needs.
for _var in $(compgen -e); do
  case "$_var" in
  ONEVCS_* | GH_* | GITHUB_* | GIT_*) unset "$_var" ;;
  esac
done
unset _var

# Pin every input a commit hash is made of. The normalizer maps whatever still
# varies (a provenance trailer carries the session token, which is minted fresh
# every run), but a world whose own commits are fixed leaves it far less to do.
export GIT_AUTHOR_NAME="Dana Okafor"
export GIT_AUTHOR_EMAIL="dana@example.invalid"
export GIT_COMMITTER_NAME="Dana Okafor"
export GIT_COMMITTER_EMAIL="dana@example.invalid"
export GIT_AUTHOR_DATE="2026-03-02T09:15:00+00:00"
export GIT_COMMITTER_DATE="2026-03-02T09:15:00+00:00"

rm -rf "$WORK"
mkdir -p "$WORK/scenes" "$WORK/hero" "$WORK/raw" "$WORK/tmp"

# --- The scratch host ---------------------------------------------------------

WORLD="$WORK/home"
mkdir -p "$WORLD/bin" "$WORLD/gh-state" "$WORLD/projects" "$WORLD/origins" "$WORLD/answers"
export HOME="$WORLD"
export ONEVCS_HOME="$WORLD/.onevcs"
export ONEVCS_GH="$WORLD/bin/gh"
export ONEVCS_FAKE_GH_STATE="$WORLD/gh-state"
# The substituted host answers at once, so a production poll interval would only
# make the capture slow; the timeouts stay generous so a loaded machine still
# captures rather than photographing a bound being hit.
export ONEVCS_LOCK_TIMEOUT_SECONDS=60
export ONEVCS_CHECKS_POLL_SECONDS=0.02
export ONEVCS_CHECKS_TIMEOUT_SECONDS=60
mkdir -p "$ONEVCS_HOME"

# git 2.47 and later follow a commit with a detached `git maintenance run --auto`
# that works inside the checkout, and a `session close` that finds a process in its
# run root rightly refuses. Off, for the reason world.rs turns it off.
cat >"$WORLD/.gitconfig" <<'GITCONFIG'
[user]
	name = Dana Okafor
	email = dana@example.invalid
[init]
	defaultBranch = main
[commit]
	gpgsign = false
[advice]
	detachedHead = false
[maintenance]
	auto = false
GITCONFIG

install -m 0755 "$REPO_ROOT/crates/onevcs/tests/fixtures/gh" "$WORLD/bin/gh"

onevcs() { "$BIN" "$@"; }

# A real bare origin with one commit on `main`, and a real clone of it.
repository() {
  local name="$1" seed="$WORLD/seed-$1"
  mkdir -p "$seed"
  git -C "$seed" init -q -b main
  printf '# %s\n' "$name" >"$seed/README.md"
  git -C "$seed" add -A
  git -C "$seed" commit -q -m "chore: seed the repository"
  git -C "$WORLD" init -q --bare "$WORLD/origins/$name.git"
  git -C "$seed" remote add origin "$WORLD/origins/$name.git"
  git -C "$seed" push -q origin main
  rm -rf "$seed"
  git -C "$WORLD" clone -q "$WORLD/origins/$name.git" "$WORLD/projects/$name"
}

# The token and worktree a `session open` printed.
field() { sed 's/.*"'"$1"'":"\([^"]*\)".*/\1/'; }

repository widgets
repository platform
repository toolbox

onevcs register "$WORLD/projects/widgets" \
  --origin https://github.com/acme-corp/widgets.git >/dev/null
onevcs register "$WORLD/projects/platform" \
  --origin https://github.com/acme-corp/platform.git >/dev/null
onevcs register "$WORLD/projects/toolbox" >/dev/null 2>&1

# The three host files the scenes are routed by. One hosted repository lands through
# the host's own auto-merge, one opens a change and waits for review, and one is a
# plain git remote with no host at all — which is the routing `rules check` and
# `repos --audit-gates` are pictures of.
cat >"$ONEVCS_HOME/rules.yml" <<'RULES'
version: 3
trailer_prefix: Onevcs-
rules:
  - match: {host: github.com, owner: acme-corp, name: widgets}
    publication: change-auto
    approvals: none
  - match: {host: github.com, owner: acme-corp, name: "*"}
    publication: change-open
    approvals: required
  - match: {path: "*/projects/toolbox"}
    publication: local-direct
    approvals: none
default: {publication: change-open, approvals: required}
RULES

cat >"$ONEVCS_HOME/workspaces.yml" <<'WORKSPACES'
version: 1
rules:
  - match: {host: github.com, owner: acme-corp, name: widgets}
    pool: 2
    overflow: 4
    delete: [".logs/"]
    maintain: {command: ["git", "gc", "--quiet"], timeout: 30m}
default: {pool: 0, overflow: unlimited, delete: []}
WORKSPACES

# The host says only how much this repository's releases are worth waiting for; the
# targets themselves come from the repository's own release-targets.toml below, which
# is the layer a consumer reads. Each is answered by the declaration's own probe
# script, given that target's identifier, run as a plain subprocess under its bound
# with no credential — here it reads a file, so nothing reaches a registry.
cat >"$ONEVCS_HOME/releases.yml" <<'RELEASES'
version: 1
default:
  adoption: fast
repositories:
  - match: {host: github.com, owner: acme-corp, name: widgets}
    adoption: published
    default_target: crate
RELEASES

printf 'crate:widgets 1.4.0\nnpm:widgets-cli 1.3.2\n' >"$WORLD/answers/released"

# What the substituted host reports as the change request's checks, and when it
# changes its mind. The hero opens with one required check still running and one
# green; from the third reading of the rollup on, the running one has passed — which
# is what a publication under a check-gated policy actually watches, and what the
# `change-check` events in the hero's stream are of. Counted in readings rather than
# in seconds, so the scene is the same on a loaded machine.
printf 'build|completed|success|true\ntest|in_progress||true\n' \
  >"$WORLD/gh-state/checks.rows"
printf 'build|completed|success|true\ntest|completed|success|true\n' \
  >"$WORLD/gh-state/checks.rows.next"
printf '2\n' >"$WORLD/gh-state/checks-flip-after"

# A real executable pre-push hook on the publication checkout: this is what `onevcs`
# runs as the merge path's verification, and what `repos --audit-gates` reports as
# that identity's coverage.
mkdir -p "$WORLD/projects/widgets/.githooks" "$WORLD/projects/widgets/scripts"
cat >"$WORLD/projects/widgets/.githooks/pre-push" <<'HOOK'
#!/usr/bin/env bash
set -euo pipefail
echo "widgets: cargo nextest run --workspace"
echo "widgets: 148 tests run: 148 passed, 0 skipped"
HOOK
cat >"$WORLD/projects/widgets/scripts/release-probe.sh" <<'PROBE'
#!/usr/bin/env bash
# What is released right now for the one registry-qualified identifier given, or
# nothing at all when that target has had no release yet. The real one asks a public
# registry; this one reads a file, so the capture stays offline.
set -euo pipefail
awk -v want="${1:?an identifier}" '$1 == want { print $2 }' "$HOME/answers/released"
PROBE
cat >"$WORLD/projects/widgets/release-targets.toml" <<'DECLARATION'
# What this repository publishes, and the only identifiers its probe answers for.
schema_version = 1
probe = "scripts/release-probe.sh"

[[target]]
id = "crate:widgets"
name = "crate"
what = "The widgets library and binary, as a Rust dependent takes them."
published_by = ".github/workflows/release.yml, the publish-crate job."
manifest = "Cargo.toml"

[[target]]
id = "npm:widgets-cli"
name = "npm"
what = "The widgets binary as an npm-installable launcher."
published_by = ".github/workflows/release.yml, the publish-npm job."
manifest = "package.json"
DECLARATION
chmod +x "$WORLD/projects/widgets/.githooks/pre-push" \
  "$WORLD/projects/widgets/scripts/release-probe.sh"
git -C "$WORLD/projects/widgets" config core.hooksPath .githooks
git -C "$WORLD/projects/widgets" add -A
git -C "$WORLD/projects/widgets" commit -q -m "ci: verify a publication with the test suite"
git -C "$WORLD/projects/widgets" push -q origin main

# --- The hero: one change's whole life ----------------------------------------
# `session open`, a commit, then `publish` under a check-gated policy while
# `events --follow` drains the stream the publication is writing into. The GIF is
# rendered from exactly these outputs.

# The hero's beats, in the order a terminal shows them: the command line as it is
# typed, and the files holding what appeared under it. `<file>:stream` is output that
# arrived over time from the backgrounded tail, `<file>:out` is a command's own
# answer; the renderer draws the two differently.
hero_steps="$WORK/hero/steps.tsv"
: >"$hero_steps"
hero() {  # hero <step> <command-line-as-shown> [<file>:<role>,...]
  printf '%s\t%s\t%s\n' "$1" "$2" "${3:-}" >>"$hero_steps"
}

echo "$WORLD/origins/widgets.git" >"$WORLD/gh-state/origin"

hero open 'onevcs session open widgets --branch feature/retry-budget' open.txt:out
onevcs session open widgets --branch feature/retry-budget >"$WORK/raw/hero-open.txt"
HERO_TOKEN="$(field token <"$WORK/raw/hero-open.txt")"
HERO_TREE="$(field worktree <"$WORK/raw/hero-open.txt")"

hero commit 'git -C "$worktree" commit -am "feat(retry): give every dispatch a retry budget"' commit.txt:out
cat >"$HERO_TREE/retry.rs" <<'SOURCE'
pub struct RetryBudget {
    pub attempts: u32,
}
SOURCE
git -C "$HERO_TREE" add -A
git -C "$HERO_TREE" commit -q -m "feat(retry): give every dispatch a retry budget"
git -C "$HERO_TREE" --no-pager log --oneline -1 >"$WORK/raw/hero-commit.txt"

# `events --follow` is a real tail: it drains the stream and sleeps 100 ms until the
# session closes. Start it first, so what it prints is what arrived while the
# publication ran rather than a replay afterwards.
hero follow 'onevcs events "$token" --follow &'
onevcs events "$HERO_TOKEN" --follow >"$WORK/raw/hero-events.txt" 2>/dev/null &
follow_pid=$!

# The tail is backgrounded, so from here the stream and the publication's own answer
# land in the one terminal together — which is what the hero is a picture of.
hero publish 'onevcs publish "$token"' events.txt:stream,publish.txt:out
onevcs publish "$HERO_TOKEN" >"$WORK/raw/hero-publish.txt"

# The follow ends itself when the session closes, which `publish` has just done.
wait "$follow_pid" || true

# --- The remaining scenes -----------------------------------------------------

# `status` is at its fullest over a change that is still open with its checks part
# way through, so it is asked about the review-gated repository rather than the one
# that just auto-merged. Its host reports three states in one reading: one required
# check green, one required check still running, and one advisory check that failed.
rm -f "$WORLD/gh-state/checks.rows.next" "$WORLD/gh-state/checks-flip-after"
printf 'build|completed|success|true\ntest|in_progress||true\nlint|completed|failure|false\n' \
  >"$WORLD/gh-state/checks.rows"
echo "$WORLD/origins/platform.git" >"$WORLD/gh-state/origin"
onevcs session open platform --branch feature/queue-depth >"$WORK/tmp/platform-open.txt"
PLATFORM_TOKEN="$(field token <"$WORK/tmp/platform-open.txt")"
PLATFORM_TREE="$(field worktree <"$WORK/tmp/platform-open.txt")"
cat >"$PLATFORM_TREE/queue.rs" <<'SOURCE'
pub fn queue_depth() -> usize {
    7
}
SOURCE
git -C "$PLATFORM_TREE" add -A
git -C "$PLATFORM_TREE" commit -q -m "feat(queue): report the depth a queue is holding"
onevcs publish "$PLATFORM_TOKEN" >/dev/null

# Work that must not be lost and is not ready to land: one branch put on its origin
# by `preserve`, and one a run left open behind it.
TOOLBOX="$WORLD/projects/toolbox"
git -C "$TOOLBOX" checkout -q -b fix/leaking-slot
printf 'the fix\n' >"$TOOLBOX/slot.rs"
git -C "$TOOLBOX" add -A
git -C "$TOOLBOX" commit -q -m "fix(pool): release a slot whose owner stopped"
git -C "$TOOLBOX" checkout -q main
onevcs preserve fix/leaking-slot --repo "$TOOLBOX" >/dev/null

onevcs session open widgets --branch feature/backoff >"$WORK/tmp/backoff-open.txt"
BACKOFF_TREE="$(field worktree <"$WORK/tmp/backoff-open.txt")"
printf 'pub fn backoff() {}\n' >"$BACKOFF_TREE/backoff.rs"
git -C "$BACKOFF_TREE" add -A
git -C "$BACKOFF_TREE" commit -q -m "feat(backoff): wait longer each time the host says no"

# A second warm slot, returned and then maintained, so `pool status` reads both
# states: one slot holding the run above, one idle with its last maintenance on it.
onevcs session open widgets --branch chore/refresh-caches >"$WORK/tmp/slot-open.txt"
SLOT_TOKEN="$(field token <"$WORK/tmp/slot-open.txt")"
onevcs session close "$SLOT_TOKEN" >/dev/null
onevcs pool maintain widgets >/dev/null

# The release that carries the hero's landing then happens: the probe's answer moves
# past the baseline the landing captured, which is what turns `release status` from
# "not released yet" into the version a consumer may now depend on.
printf 'crate:widgets 1.5.0\nnpm:widgets-cli 1.3.2\n' >"$WORLD/answers/released"

# --- Render each scene to raw text --------------------------------------------

scene() {  # scene <name> <command...>
  local name="$1"
  shift
  "$@" >"$WORK/raw/$name.txt" 2>/dev/null
}

# clap styles `--help` — underlined section headings, emphasized literals — only when
# it is writing to a terminal, and this CLI has no `--color` flag to ask with. So the
# capture gives it one: `script` runs the binary under a pty, and the `\r` a pty adds
# is dropped afterwards. The text itself does not depend on the pty's size (clap's
# `wrap_help` is off), so the scene is the same in any terminal.
TERM=xterm-256color script -qec "$BIN --help" /dev/null 2>/dev/null |
  tr -d '\r' >"$WORK/raw/help.txt"

scene rules-check onevcs rules check widgets
scene audit-gates onevcs repos --audit-gates
scene status onevcs status "$PLATFORM_TOKEN"
scene recoverable onevcs recoverable --all
scene pool-status onevcs pool status widgets
scene release-targets onevcs release targets widgets

# `release status` is deliberately NOT a scene. Its whole answer is one line —
# `released: crate 1.5.0 (automated, probed)` — and a one-line picture says strictly
# less than the paragraph it would sit beside. screenshots/AGENTS.md records the
# judgement, and the README shows that line as text instead.
onevcs release status "$HERO_TOKEN" >/dev/null

# --- Normalize, all in one pass so the placeholder map is shared ---------------

awk -f "$REPO_ROOT/scripts/screenshots-normalize.awk" -v root="$WORLD" "$WORK"/raw/*.txt

for raw in "$WORK"/raw/*.txt.norm; do
  base="$(basename "$raw" .txt.norm)"
  case "$base" in
  hero-*) mv "$raw" "$WORK/hero/${base#hero-}.txt" ;;
  *) mv "$raw" "$WORK/scenes/$base.txt" ;;
  esac
done
rm -f "$WORK"/raw/*.txt

# Nothing below this line may be empty: an empty scene is a command that failed, and
# `freeze` would render it as a blank window rather than refuse.
for scene in "$WORK"/scenes/*.txt "$WORK"/hero/*.txt; do
  [ -s "$scene" ] || {
    echo "screenshots-capture: $scene is empty — the command that fills it failed" >&2
    exit 1
  }
done

echo "screenshots-capture: $(ls "$WORK/scenes" | wc -l) scenes in $WORK/scenes" >&2
