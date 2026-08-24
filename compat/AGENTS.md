# The compatibility project

Instructions that are true of `compat/` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

## What it is for

One claim, which the crate next door cannot make about itself: that a build of
`onevcs` **already in the field** goes on reading the streams this build writes.
The envelope types are duplicated per repository by design, so asserting that from
the current sources would only ask this build about itself. So the dependency here
is the released crate from crates.io, at a pinned version, and the fixture is the
one `docs/contract.md` declares — the same document `crates/onevcs/tests/contract.rs`
holds this build's own serialization to, so the two ends meet on one text rather
than on a copy of it.

Pin the version **exactly**. What is proved is a property of *that* build, and a
range that quietly moved would change what was proved without anyone deciding to.

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
lockfile, and its build lands under `target/compat` so there is no second directory
to clean. `nx.json` names `compat/**/*` among the crate test target's inputs, so a
change here re-runs it rather than replaying a cached pass.
