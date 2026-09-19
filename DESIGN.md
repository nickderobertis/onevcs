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

## Every process emits the same envelope, and the types are `onemessagebus`'s

`oneagentgraph`, `onevcs`, and `onepipeline` all emit the same NDJSON envelope.
Each once kept its own copy of the types, and the copies drifted: this crate's
matcher grew a `phase` the other two never had. So the envelope, its filter
grammar, the payload bound and the redaction tables were extracted into
`onemessagebus` and its agent profile, `onemessagebus-agent`, and `onevcs` is their
first consumer. It re-exports them at the paths it always had and keeps its own
vocabulary — `EventKind`, and the phase each kind belongs to. The contract test
still asserts this crate's serialization against the fixtures in
`docs/contract.md`, now through the re-exported types, so a bus that stopped writing
those bytes fails here.

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

## A pool of warm worktree slots, per identity, per host

The user's words: "some languages like rust have large build outputs that put a lot
of wear on the disc to repeatedly wipe and recreate"; the pool lets "you trade off
disc wear for not blocking work. some users may turn overflow off to save their disk
and others have overflow on"; "most users who are working in languages that have
these problems would want to set the pool size to the amount of concurrent work that
they expect to do on that project and have a small overflow to prevent blocking
work"; "much like a database pool"; "the pool should also be lazy ... a reasonable
default say 2, then projects that only ever have one concurrent task will only ever
get one work tree"; "this all lives on the host per identity because each host will
be able to handle different amounts of concurrency mainly based on available disk
space".

So `$ONEVCS_HOME/workspaces.yml` sizes a pool and an overflow per identity, on the
host; a `session open` takes an idle slot, cuts one while the pool is under its
size, overflows into a disposable run root, or is refused with `PoolExhausted` and
never waits; a `session close` returns the slot with its ignored files — the
repository's own declaration of what is build output — still there. Pooling is off
until a host writes the file, so every existing host is unchanged. The shipped
default is `pool: 0` rather than the user's "reasonable default say 2" because the
file is what turns pooling on: a host that has said nothing keeps today's behaviour,
and the moment it writes the file it says its own number.

**A slot is bound to its lender.** The manager's ruling: re-pointing a slot's clone at
another execution checkout is refused, because a branch the hand-back could not copy
may reference objects only the old lender holds. An identity with several execution
checkouts spends its pool one slot per lender.

**Surplus shedding measures against the file's pool, never a per-process override.**
The manager's ruling on a literal reading that would have `session open --pool 0`
shed every idle slot before placing one session fresh: `--pool` and `ONEVCS_POOL`
govern only where *this* session is placed (`0` fresh under `runs/`, `N` may cut a
slot while fewer than `N` exist), `--overflow` and `ONEVCS_OVERFLOW` only this open's
admission, and a slot is removed only by shedding against the file's pool at an open,
or by `pool prune`, and never one holding a retained branch.

**The idle proof is the records', and there is no second liveness test.** A slot is
idle iff no open session record names it and its maintenance claim is void. An open
record whose owner exited holds its slot until `session close` or `onevcs sweep`
forgets it — the same proof the run-root reclamation uses, for the same reason: a
session opened from the command line has no owner process from the instant its token
is printed.

**Nothing about nodes or queues enters the library.** The refusal names the identity,
the limits with their sources and every holder, and a caller decides whether to wait,
close something, or open with `--overflow unlimited`.
