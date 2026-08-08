# DESIGN.md

Durable design decisions **the user stated**, kept because the code shows what was
built and never the constraint that drove it. Nothing an agent decided on its own
belongs here — an inference this repository had to make is recorded with the rest
of them, not mixed in with the user's direction.

## The contract is fixed; the interface is not negotiable in passing

`docs/contract.md` was approved before any code existed and is committed verbatim.
A conflict inside it is reported to the planner as a proposal, never resolved by
editing the interface — because two other repositories are being built against
this surface at the same time, and a unilateral change to it breaks them silently.

## The review unit is host-neutral

The vocabulary is a **change request**, not a pull request, and stack metadata is
`change_url` / `change_base`. GitHub is the only host implemented; GitLab is
expected later, and naming the concept after one host is what would have to be
undone to add the second.

## Every process emits the same envelope, and the types are duplicated

`oneagentgraph`, `onevcs`, and `onepipeline` all emit the same NDJSON envelope,
and each one **duplicates** the types rather than sharing a util crate. That is
deliberate: a shared crate would couple three release trains, and the drift it
would prevent is instead prevented by each repository's contract test asserting
its own serialization against the fixtures in `docs/contract.md`.

## Dependency direction is one-way

`onepipeline` depends on `oneagentgraph` and `onevcs`; `onepipeline-ui` depends on
`onepipeline`. Nothing depends back. `onevcs` therefore knows nothing about
pipelines or agents — only about repositories, hosts, and its own events.

## release-plz is the single version driver

The version lives in `crates/onevcs/Cargo.toml` and nowhere else. The wheel takes
it through maturin's `dynamic = ["version"]` and the npm packages through
`scripts/npm-build.mjs`, so no human and no second manifest can disagree with the
tag that was cut.
