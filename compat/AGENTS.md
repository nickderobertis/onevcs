# The compatibility project

Instructions that are true of `compat/` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

## What it is for

A claim the crate next door cannot make about itself: that a pinned build of
`onevcs` **already in the field** goes on reading what this build writes — its
streams, and the state it leaves on a host they share — as far as that build's own
readers reach.
A released build carries its own copy of the envelope types, from before they were
`onemessagebus`'s, so asserting that from the current sources would only ask this
build about itself. So the dependency here
is the released crate from crates.io, at a pinned version, and the fixture is the
one `docs/contract.md` declares — the same document `crates/onevcs/tests/contract.rs`
holds this build's own serialization to, so the two ends meet on one text rather
than on a copy of it.

How far a release's readers reach is a property of the release, and the claims below
say which. An added envelope *field* is proved read by 0.13.0. An added event *kind*
is not: 0.13.0 and 0.14.x carry `kind` as a closed `EventKind` enum with no
`#[serde(other)]`, so a line of a kind added after them fails as an unknown variant.
Passing over an unknown kind entered the released `EventStream` in 0.15.0, and
`EventLines`, public only since 0.31.0, has done the same since then; both are proved
here for the pinned 0.32.2. A consumer on a release before 0.15.0 has to upgrade
before a producer writes a new kind.

Pin the version **exactly**. What is proved is a property of *that* build, and a
range that quietly moved would change what was proved without anyone deciding to.

It makes four claims now, against two released builds, and each is the build the
claim is about:

- `tests/released.rs` holds **0.13.0** — a build from before an envelope carried
  `phase` — to reading the envelope this build stamps one on. It is linked as
  `onevcs-envelope-era`, because only a build that old can make that claim. The
  claim is about an added field and nothing more: its `EventKind` is closed, so its
  vocabulary bounds the kinds the journey feeds it.
- `tests/unknown_kind.rs` holds **0.32.2** to reading a stream this build wrote in
  which an envelope of a kind 0.32.2 has no word for — read from this build's
  `EventKind`, never spelled by the test — sits between two it knows: `EventLines`
  keeps the line and offers no envelope for it, its filtered read and `EventStream`
  leave it out, and each hands back exactly the known events.
- `tests/retired.rs` holds **0.32.2** — the release consumers ran when retirement
  landed (on 2026-09-26, the one `ai-orchestrator` pinned), so the build a consumer
  shared a host's `$ONEVCS_HOME` with — to reading the state this build leaves after
  retiring a branch: the registry, every session record, every stream (the
  `branch-superseded` and `branch-retired` kinds it has no word for included), and its
  own `recoverable` and `status` answers about a branch nothing retired, unchanged. It
  is the plain `onevcs` dependency. The version is a decision recorded here, not a
  mirror of any other repository's file: this project is offline and cannot read one,
  and the claim stays true of 0.32.2 whatever a consumer later pins. Moving it to a
  newer release is a new claim someone decides on, never a sync. It is **Unix only**,
  as every retirement journey in `crates/onevcs/tests/e2e` is: on Windows this build's
  `retire` keeps the landed branch as `unknown` rather than retiring it, so the
  journey's premise never holds there. The file's head says where that was observed,
  and its `diagnosis` is what a failed retirement prints, so the day it is re-enabled
  on Windows it names the read that failed.
- `tests/verdicts.rs` holds both to a state root on which this build's
  finished-branches pass has recorded verdicts under `$ONEVCS_HOME/verdicts/`. The pass
  moves nothing an older build reads — the registry and every session record stay the
  bytes they were, at versions 6 and 3 — and **0.32.2** answers every read it makes
  (the registry, the session records, `recoverable`, `status`, every stream) exactly as
  before. **0.13.0 already refuses the base's registry**: it reads registry versions 2
  to 5 and parses a later one as version 5's shape, and version 6 dropped two fields
  that shape requires, so every verb of it that loads the registry fails on a host
  this build registered. That journey therefore holds it to something narrower and
  exact: every answer it gives — that refusal included — is byte-identical before and
  after the records are written. Unix only, for the reason `retired.rs` is.

The 0.32.2 claims need this build to *write* what is read, so this build is linked
too, from the path beside it, as `onevcs-current`: each journey writes with it and
reads back with the release, both through their libraries, in one process — which is
why each sets `ONEVCS_HOME` (and the retirement journey `HOME`) in its own process and
relies on nextest's process per test.

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
or checked for unused dependencies. Its dependencies are two published builds of
this crate (0.32.2 and 0.13.0), this crate itself by path, and `serde_json`; adding
another means saying so here. Three packages named `onevcs` resolve in this graph,
which is harmless *here* — nothing runs `--package onevcs` against this manifest —
and is one more reason it stays out of the workspace next door.

## How it is run

`just _crate-compat`, which `_crate-test` and `test-quick` call, so it is inside
`just check` and `just gate` like everything else. `_crate-fmt-check` and
`_crate-lint` hold it to the same bar, `just bootstrap` fetches its committed
lockfile, and its build lands in the clone's own `target` — `.cargo/config.toml`
reaches every crate under the clone — so there is no second directory to clean.
Because it links this crate by path, its lockfile records this crate's version, and
a release bump that release-plz makes leaves it stale; the release job carries it
along with `scripts/release-pr-carry.sh`, and `just release-pr-check` proves a
release PR's tree still bootstraps.
`nx.json` names `compat/**/*` among the crate test target's inputs, so a change
here re-runs it rather than replaying a cached pass.
