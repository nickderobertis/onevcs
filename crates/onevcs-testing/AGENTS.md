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

Publishing is where that rule does the most work, because a publication reaches
both interfaces. The **host** side of one is performed here: the change request is
really opened on the `Hosting` the publication was handed, really adopted when the
host already holds one, and really merged under the policy. The **repository** side
is not, and none of it is claimed — there is no origin to fetch, no tree to run a
gate in, nothing to push, and no lock to queue behind, so a publication here emits
`change-opened`, `change-merged`, and `merge-completed` and never `fetch`,
`gate-started`, `gate-verdict`, `push`, `lock-wait`, `lock-acquired`, or
`merge-queued`. Two more things it cannot read, and states instead of inventing:
the policy comes from `VcsState::policy` rather than a rules file (narrowed through
`MergePolicy::narrow`, which is the rules system's own rule), and an unrequested
change-request title names the branch rather than a commit subject there is no
commit to take. A *requested* title needs no check here — `PublishRequest::title`
is a `Subject`, so one that could not be a commit subject never reaches a
provider.

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

## A file-backed state is a stored contract

`FileVcs` and `FileHost` write a document that outlives the process that wrote it,
so it is the same kind of thing as `onevcs`'s registry document and carries the
same three obligations. `STATE_VERSION` is the version this build **writes** and
`OLDEST_READABLE_VERSION..=STATE_VERSION` is what it **reads**, with anything
outside that range refused by name (`readable_version`); the bytes are checked in
under `tests/golden/` and compared **byte for byte** by `golden.rs`; and every
field but the version is `skip_serializing_if`-empty, so a hand-written scenario
names only what it means and a document from a build that knew fewer fields still
reads.

**Every change to the document is versioned, an added optional field included.**
Serde would read the older document either way, so the bump buys nothing at the
parser and everything at the boundary it is for: the version is how a consumer's
checked-in scenario says which shape it was written against, and a document that
grew a field without saying so leaves nothing able to tell "this build wrote
nothing there" from "this document predates the field".

Compatible and unremarkable are not the same thing, so the *range* is what carries
compatibility. A version is readable rather than refused when every field it holds
still means here what it meant there — an added field keeps that true, a changed
meaning does not, which is why version 1 is refused: its sessions would every one
read back as open, a wrong answer where a journey asserting on a session it had
closed needs a refusal. `Checked::carry_forward` is the other half, on read, so
nothing past that boundary asks which version a document arrived as and the next
write declares the version it now is. `check_sources` is an `Option` for a related
reason: unset and explicitly empty are two different scenarios (a credential nobody
narrowed, and one that can read no source at all and must refuse), and a set whose
emptiness meant "not stated" could not express the second.

Changing the document therefore means, in the same change: bump `STATE_VERSION`,
regenerate both goldens under their new version's names, **freeze the previous
version's documents beside them** (`*-v<n-1>.json`, which are not goldens — nothing
writes them any more — but the consumer's checked-in scenario that `golden.rs` reads
forward), say whether the old version is readable or refused and why, and teach
`carry_forward` what it becomes. The goldens are generated by the writer itself
rather than typed — seed a provider at `tests/golden/<name>.json` and let
`FileStore::save` produce them — so the fixture cannot disagree with the code that
writes it.

## Deterministic, because a journey has to be able to name what it asserts on

Session tokens are `s-testing-N`, change requests number from 1, artifact ids are
derived from what they are a log *of*, and every hash-shaped value is a digest of
its inputs. A journey can therefore seed the checks of a change request it has not
opened yet. Keep it that way: a clock or a counter that is not part of the state
takes that away.
