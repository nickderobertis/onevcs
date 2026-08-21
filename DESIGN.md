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

## The publication subject limit is the operator's, and it has one source

The operator raised it. The old value was the width a wrapped commit *body* is
written to, which was never their rule for a subject, and it twice refused
complete, verified work at publication; a description cut to fit is not on offer,
so the refusal was the whole cost.

`provenance::SUBJECT_LIMIT` is the only statement of the number, and `onepipeline`
reads `onevcs::provenance::SUBJECT_LIMIT` at plan load to ask the same question
this crate asks at publication rather than restating a value that drifts the first
time it moves. That consumer is why the module is public at all: **`SUBJECT_LIMIT`
is the only item of `crates/onevcs/src/provenance.rs` that is not `pub(crate)`**,
and its name and path are fixed.

## Nothing may open a change request for a branch with nothing to merge

Three measured incidents across two repositories, all one shape: the session's work
reached the base under somebody *else's* change request, and publishing the session
afterwards opened a change request whose diff was empty. Every path-filtered
required check skipped rather than ran, the host held the change BLOCKED with
nothing left that could unblock it, and the node reported failure for work that had
already shipped. So a publication asks the *tree*, not the history — a branch that
landed keeps every one of its commits — and settles as `NothingToPublish`. The
question is asked on the publication path itself rather than at a call site, so
every caller gets it.
