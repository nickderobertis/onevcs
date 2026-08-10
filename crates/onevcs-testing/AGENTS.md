# The testing crate

Instructions that are true of `crates/onevcs-testing` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

## Nothing here may claim an operation it did not perform

This crate exists so a consumer can drive a real `onevcs` without a real GitHub.
Everything it is worth rests on one property: what it emits is what the real
implementation emits. A provider that performs an operation *silently* leaves a
consumer's suite proving an event stream nobody produces; a provider that emits an
event for work it did not do is worse, because the stream then looks richer than
the truth. Both are drift, and both are caught by `honesty.rs` in the crate next
door, which runs one publication and one session journey against `Git` + `GitHub`
and against these, and holds the two streams to each other.

Two things follow when you add a behaviour here:

- **Read the real implementation first**, down to which labels an event carries.
  `commit-preserved` carries no `identity` label because the real one does not:
  the label is stamped where a session is opened, and preserving work happens
  against a stream a later process opened fresh.
- **When the real implementation does something a provider cannot**, do not
  approximate it. A `fetch` is git's own housekeeping against a remote no provider
  has, so no provider emits one — and the comparison excludes that kind by name,
  with the reason written where it is excluded.

## One behaviour, two stores

`MemoryVcs` / `FileVcs` and `MemoryHost` / `FileHost` are type aliases over one
`Repository<T>` and one `Host<T>`. That is deliberate: a behaviour taught to the
in-memory flavour and forgotten on the file-backed one is exactly the kind of gap
nobody finds. Put behaviour in the generic implementation and let the store differ.

`FileStore::attach` keeps what is already at the path and `replace` overwrites it,
which is why `create` attaches and `seeded` replaces: a second provider over the
same document is meant to be the *same* state.

## The state root is duplicated here, deliberately

Events go to `$ONEVCS_HOME/streams/<token>.ndjson` and artifacts to
`$ONEVCS_HOME/artifacts/<id>`, resolved the way `onevcs` resolves them, because
`onevcs` exposes no writer and the contract's envelope types are duplicated per
crate by design. **In-memory means the provider's state, never the stream**: both
flavours emit the same way, because the stream is the thing under test rather than
part of the bookkeeping. Nothing here can drift silently — writing anywhere else
produces a stream `onevcs events` cannot read, and the honesty gate reads both
runs through that command.

## Deterministic, because a journey has to be able to name what it asserts on

Session tokens are `s-testing-N`, change requests number from 1, artifact ids are
derived from what they are a log *of*, and every hash-shaped value is a digest of
its inputs. A journey can therefore seed the checks of a change request it has not
opened yet. Keep it that way: a clock or a counter that is not part of the state
takes that away.
