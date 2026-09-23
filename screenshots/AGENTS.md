# Terminal screenshots

Deterministic SVGs of `onevcs`'s **real** output, plus one animated GIF, gated by
[screencomp](https://github.com/nickderobertis/screencomp). Informational — **never
part of `just check`, `just gate`, or CI's `gate` job**; the `Visual docs` workflow
(`.github/workflows/visual-docs.yml`) owns the comparison on pull requests, alongside
`deps-check`, `semver-check`, `smoke-real` and the llmlint tier.

> `CLAUDE.md` here is a symlink to this file — edit `AGENTS.md` only.

## What it is

`scripts/screenshots-capture.sh` drives the **real release `onevcs` binary** against a
scratch host it builds the way `crates/onevcs/tests/e2e/world.rs` builds one: real bare
origins on disk, real clones, a real executable `pre-push` hook git runs, real pushes
into real origins. The one thing substituted is the remote host's *decisioning* —
which change requests exist, what their checks say, whether a merge may proceed — and
the program that answers it is **literally the e2e tier's own**
(`crates/onevcs/tests/fixtures/gh`, installed as `gh` through `ONEVCS_GH`). One file,
both callers: a stand-in copied into the capture would be a second host whose answers
drift from the ones the journeys are written against.

So there is no model, no network, no credential and no real GitHub anywhere in a
capture. `scripts/screenshots.sh` renders what the driver wrote to SVGs with
[`freeze`](https://github.com/charmbracelet/freeze); `scripts/demo-gif.py` renders the
hero GIF from the same driver's transcript.

## The scenes, and what each documents

The scratch host carries three registered repositories, because the routing is the
thing most of these reports are about: `widgets` (`github.com/acme-corp/widgets`,
publishing through the host's own auto-merge), `platform` (the same host, opening a
change request and waiting for review), and `toolbox` (a plain git remote with no host
at all).

- **`help`** — the whole command surface. `## Use` in the README was one `onevcs
  --help` line showing nothing at all; this is that line's answer. clap styles its help
  (underlined section headings, emphasized literals) only when it is writing to a
  terminal, and this CLI has no `--color` flag to ask with, so the capture runs it
  under a pty with `script(1)`. It is deliberately **not** the hero: it documents the
  surface, it does not show the tool working.
- **`rules-check`** — `onevcs rules check widgets`: the identity a checkout resolved
  to, the rules file consulted, *which rule matched*, and the publication and approvals
  it decided, each with its source. It sits beside the prose about the rules file,
  which is otherwise the hardest thing in the README to picture.
- **`audit-gates`** — `onevcs repos --audit-gates`: all three identities at once, each
  with its resolved policy, the host's required checks, and what covers its merge path
  — a `pre-push` hook for one, the host's checks for another, and *nothing* for the
  third. The per-identity gate and required-check reading, which is what that section
  of the README describes.
- **`status`** — `onevcs status REF` over a change request that is still open with its
  checks part way through, which is where the report is at its fullest: all five
  sections (`work`, `identity`, `session`, `branch`, `publication`) plus the checks
  table — one required check green, one still running, one advisory one failed — the
  merge path's verdict, and the `next:` line saying what advances the work.
- **`recoverable`** — `onevcs recoverable --all`, by far the most narrative report the
  tool has: a scope header, then a per-branch line with its identity and marks, and the
  indented `Found in:`, `On origin:`, `Stopped because:`, `Landed:` and pasteable
  `Resume:` lines. `--all` rather than the default, because it is the only form in
  which the `Landed:` line appears, and the three branches are chosen so that every one
  of those line kinds shows: one preserved onto its origin, one a stopped run left
  open, one that reached its base.
- **`pool-status`** — `onevcs pool status widgets`: the capacity, and both slot states
  — one holding a live session, one idle with the outcome and time of its last
  maintenance on it.
- **`release-targets`** — `onevcs release targets widgets`: the adoption a host chose
  for a repository, the declaration it read out of that repository's own
  `release-targets.toml`, and each target with its style, its probe and where the
  target came from.

### Two candidate scenes are deliberately absent

The rule is that a shot has to say more than the paragraph it would sit beside.

- **`sweep --dry-run`** is dropped. Its report is three quarters standing prose about
  the families it does *not* examine — the same sentences the README already writes —
  and the two things that are not prose cannot be captured honestly: the reclaimed
  size in bytes is whatever the host's git happened to pack, and each run root's name
  carries a digest of its own absolute path. Normalizing those would put two numbers in
  a picture that no run ever printed.
- **`release status`** is dropped. Its whole answer is one line — `released: crate
  1.5.0 (automated, probed)` — and a one-line picture says strictly less than the
  paragraph beside it. The README shows that line as text instead.

## The hero (`docs/screenshots/demo.gif`)

The README's first image is an **animated GIF of one change's whole life**: `session
open`, a commit, then `publish` under a check-gated policy while `onevcs events
"$token" --follow` drains the stream the publication is writing into. That pair is the
tool's purpose in motion — `events --follow` is a real tail (it drains and sleeps
100 ms until the session closes), and a publication under that policy blocks and polls
the host's checks, emitting `change-check` events into the same stream. The substituted
host is told to report one required check still running and then, from the third
reading of the rollup on, to report it passed — counted in *readings* rather than in
seconds, so the scene is the same on a loaded machine. A still of that text says
strictly less.

What is reconstructed rather than screen-recorded is the *arrival*: capturing a live
PTY hermetically would need `ttyd`/`ffmpeg` and would not be reproducible anyway, so
the frames a terminal would draw are rebuilt from that run's real output, line by line,
in the order it arrived, and rendered with the same vendored font. Like `llmlint`'s, the
GIF is **not** hash-gated — a GIF is not byte-reproducible across Pillow versions — so
it is regenerated on demand with `just screenshots-gif` and committed.

## Why the capture is byte-reproducible

screencomp gates on the **hash** of each image, so two captures of one build must
produce byte-identical trees. Unlike a rasterized PNG (whose anti-aliasing drifts
across CPUs), an SVG is pure layout maths, which is why this needs no container and
earns a single `x86_64` lane whose committed baseline is correct on every host. What is
pinned, and where its one source is:

| pinned | its one source | how a copy is kept honest |
| --- | --- | --- |
| the `freeze` release | `freeze_version` in `scripts/ci-install-freeze.sh` | the justfile's `freeze-version` reads it back out of that script, so the local install and CI's install share one pin |
| the renderer's font | the vendored file itself, `fonts/JetBrainsMono-Regular.ttf` (OFL — see `fonts/JetBrainsMono-OFL.txt`) | it is stated once, in `scripts/screenshots.sh`'s `--font.file`; there is no version string to drift |
| the arch lane | `[capture].arches` in `screencomp.toml` | `scripts/host-arch.sh` is the one place a host's own lane name is derived from `uname -m`, shared by the capture, the bless and the guard; the guard reads the declared list back out of `screencomp.toml` and refuses a host it does not name |
| the screencomp release | `.github/workflows/visual-docs.yml`, which names it twice (the reusable workflow's `uses:` ref and its `screencomp-version:` input) | `screencomp doctor --env` reconciles both against the installed CLI and reports a drifted pin as a problem |
| the Rust toolchain | `rust-toolchain.toml` | the workflow names no Rust version at all: the container's rustup reads that file and installs what it pins |

The font is passed with `--font.file` and embedded into each SVG as base64, so `freeze`
never fetches one over the network and the file renders the same on GitHub and
crates.io with nothing external to load.

**The environment is cleared.** `onevcs` reads its own `ONEVCS_*` settings ahead of
almost everything else, so an exported one steers the scenes and prints straight into
them; `GH_*`/`GITHUB_*` would reach the substituted host, and `GIT_*` would move a
commit hash. `scripts/screenshots-capture.sh` unsets all four families before it sets
the handful the capture itself needs.

**Everything else is normalized** (`scripts/screenshots-normalize.awk`). There is no
clock override and no deterministic-id switch in this tool, and this machinery does not
add one — a flag added for a screenshot's convenience would be a change to
`docs/contract.md`. So the normalizer rewrites, in one pass over every scene at once:
the scratch host's absolute path (to `/home/dev`), session tokens, artifact ids, commit
hashes (the provenance trailer carries the session token, so even a fixed-date merge
hashes differently every run), RFC 3339 timestamps, process ids and elapsed times. Each
family is mapped **in order of first appearance**, not collapsed onto one value, so two
different sessions still read as two different sessions and a placeholder is the same
shape and length as what it stands in for. The world's own commits are made with a
pinned name, email and both dates, which leaves the normalizer far less to do.

`awk` must support POSIX interval expressions (`{40}`); the normalizer checks that
first and refuses rather than silently normalizing nothing. The capture is Unix-only —
real POSIX hooks and a POSIX `gh` stand-in, the same gate `world.rs` states with
`#![cfg(unix)]` — and needs util-linux `script(1)` for the `help` scene.

## Why this is not a second statement of the contract

`docs/contract.md` is the approved source for every verb's surface, and
`crates/onevcs/tests/contract.rs` holds the parser to it. A committed screenshot of CLI
output *would* be a second spelling of that contract if it were hand-made. It is not:
every image here is produced by running the real binary, and CI refuses the moment its
bytes diverge from the committed digest. The baseline **is** the drift gate, and the
binary remains the one source — these files are a rendering of the surface, not a
restatement of it. Nothing here may be edited to make a picture nicer: a shot that
contradicts the contract is a contract bug to report, not a caption to fix.

## Outputs

- `shots/current/<arch>/captures.json` + the SVGs — the capture screencomp reads
  (gitignored; regenerated). `$SHOTS_OUT` overrides the directory; the reusable
  workflow exports it per arch lane.
- `shots/baseline/<arch>.json` — the committed digest baseline (no images).
- `docs/screenshots/*.svg` and `docs/screenshots/demo.gif` — the committed copies the
  README embeds. They live in this repository and nowhere else; no gallery is published
  from here today.

## Commands

- `just screenshots-tools` — install the pinned `freeze` (needs Go). screencomp is
  installed separately (see its README); CI installs both itself.
- `just screenshots` — capture. Builds the release binary, drives it, renders the
  shots and refreshes the README copies. Quiet on success.
- `just screenshots-gif` — regenerate the animated hero (needs Python 3 + Pillow).
- `just screenshots-bless` — after an **intended** output change: recapture and rewrite
  this host's lane baseline. Commit `shots/baseline/` and `docs/screenshots/` together.

## The gate, and how the local half is switched on

CI (`fail-on-drift: true`) fails when a capture diverges from the committed baseline.
The local pre-push guard (`.githooks/pre-push`) re-captures **only** when a
`[guard].paths` file changes (`screencomp.toml`), and on drift it rewrites this host's
baseline, builds a review gallery at `shots/review/index.html`, and blocks the push so
you commit the refreshed baseline and README images deliberately.

`core.hooksPath` is per-clone state that is never committed, so a committed hook nothing
activates is an inert file. **`just bootstrap` activates it** — the `workspace` project's
`bootstrap` target runs `scripts/install-hooks.sh`, which points `core.hooksPath` at
`.githooks` (idempotent, and it refuses rather than quietly disarming a clone that had
installed hooks of its own). `screencomp doctor --env` is what says the setup is really
wired rather than merely present.

**`.githooks/` carries the screencomp guard and nothing else.** `just gate` stays
unhooked, exactly as it was before this directory existed: wiring the complete pre-push
bar into a git hook is a change to the development loop that nobody asked for. `just
gate` is still the bar you run before pushing.

## Changing the screenshots

Editing what any of these verbs prints, the CLI surface, the `gh` stand-in, or the
scenes in `scripts/screenshots-capture.sh` will change the SVGs. That is expected — run
`just screenshots-bless` and commit the new baseline together with
`docs/screenshots/`. Bumping `freeze_version` or replacing the vendored font reflows
every shot; bless once. Regenerate the GIF when the event envelope or any of the hero's
four commands change what they print.
