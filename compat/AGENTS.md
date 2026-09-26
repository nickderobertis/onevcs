# The compatibility project

Instructions that are true of `compat/` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

## What it is for

One claim, which the crate next door cannot make about itself: that a build of
`onevcs` **already in the field** goes on reading the streams this build writes.
A released build carries its own copy of the envelope types, from before they were
`onemessagebus`'s, so asserting that from the current sources would only ask this
build about itself. So the dependency here
is the released crate from crates.io, at a pinned version, and the fixture is the
one `docs/contract.md` declares — the same document `crates/onevcs/tests/contract.rs`
holds this build's own serialization to, so the two ends meet on one text rather
than on a copy of it.

Pin the version **exactly**. What is proved is a property of *that* build, and a
range that quietly moved would change what was proved without anyone deciding to.

It makes two claims now, against two released builds, and each is the build the
claim is about:

- `tests/released.rs` holds **0.13.0** — a build from before an envelope carried
  `phase` — to reading the envelope this build stamps one on. It is linked as
  `onevcs-envelope-era`, because only a build that old can make that claim.
- `tests/retired.rs` holds **0.32.2** — the release `ai-orchestrator`'s
  `config/onevcs.version` names, which is the build a consumer shares a host's
  `$ONEVCS_HOME` with — to reading the state this build leaves after retiring a
  branch: the registry, every session record, every stream (the `branch-superseded`
  and `branch-retired` kinds it has no word for included), and its own `recoverable`
  and `status` answers about a branch nothing retired, unchanged. It is the plain
  `onevcs` dependency. When that file moves to another release, move this pin with it.

The second claim needs this build to *write* the state, so this build is linked too,
from the path beside it, as `onevcs-current`: one journey writes a real scratch host
with it and reads the host back with the release, both through their libraries, in
one process — which is why the journey sets `HOME` and `ONEVCS_HOME` in its own
process and relies on nextest's process per test.

## Why it is not a workspace member, and why that must not change

Two packages named `onevcs` in one resolve graph make `--package onevcs`
ambiguous — and that spelling is on the release path:
`scripts/publish-crates.sh` runs `cargo pkgid --package onevcs` and `cargo publish
--package onevcs`, and `release.yml` builds every platform binary with it. None of
it would be caught here, because the publish journey stubs `cargo`; it would be
caught by a release that shipped nothing, which this repository has already had
twice. Moving this directory into `crates/` is therefore not a tidy-up.

The cost of being outside is exact and worth stating: `cargo deny` and `cargo
machete` are `--workspace`, so nothing here is licence-audited, advisory-audited,
or checked for unused dependencies. Its dependencies are this crate's own published
build and `serde_json`, and adding a third means saying so here.

## How it is run

`just _crate-compat`, which `_crate-test` and `test-quick` call, so it is inside
`just check` and `just gate` like everything else. `_crate-fmt-check` and
`_crate-lint` hold it to the same bar, `just bootstrap` fetches its committed
lockfile, and its build lands in the clone's own `target` — `.cargo/config.toml`
reaches every crate under the clone — so there is no second directory to clean.
`nx.json` names `compat/**/*` among the crate test target's inputs, so a change
here re-runs it rather than replaying a cached pass.
