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
`change-opened`, `change-drafted`, `draft-lifted`, `draft-lifted-early`,
`draft-kept-for-review`, `checks-settled`, `merge-armed`, `change-merged`, and
`merge-completed` and
never `fetch`, `gate-started`, `gate-verdict`, `push`, `lock-wait`, `lock-acquired`, or
`merge-queued`. What it records before a failure stays recorded, as it does next
door. Two more things it cannot read, and states instead of inventing: the policy
comes from `VcsState::policy`, `approvals` and `drafts` rather than a rules file
(narrowed through `MergePolicy::narrow`, which is the rules system's own rule), and an
unrequested
change-request title names the branch rather than a commit subject there is no
commit to take. A *requested* title needs no check here — `PublishRequest::title`
is a `Subject`, so one that could not be a commit subject never reaches a
provider.

**The draft lifecycle has no clock here, and must not grow one.** Each phase of the
watch is one reading of the host — one that has not settled stands for the bound, and
a draft nothing ran on for the grace window — so a consumer drives a row by seeding
the reading it wants —
`checks_after_lift`, `required_checks` — never by timing one. A counter or a clock
outside the state would make those rows unrepeatable. The one counter there is, is the
state's own ledger: `check_readings`, `check_readings_after_lift` and `head_activity`
answer the n-th call about a change request with their n-th entry, which is how a journey
next door drives the real watch poll by poll on either side of a lift that happens on a
clock.

The two description methods are host-side and performed: `describe_change` writes
into the same `titles` and `bodies` `open_change` wrote and records the call in
`described`, so `change_description` answers the description as it now stands
whichever call wrote it — which is what lets a consumer drive a whole closeout
(`session_change` → `describe_change` → `publish`) against these hosts and read
back what it wrote.

## A person's moves, and the ledger of every call

The host keeps `HostState::ledger`: every `RemoteHost` method it answers writes one
`HostCall` — the method, the change requests it names, and what GitHub charges the call
the real host sends (`landings.rs` holds the charges). **A method added to `RemoteHost`
records itself here and joins the list in
`the_ledger_records_a_call_to_every_method_the_host_interface_declares`**, which calls each
one and holds the ledger to naming exactly that list — a consumer sums the ledger over a
whole review round, and a method missing from it is a cost nobody sees. `mergeability` and
`visibility` are implemented only to be recorded: they answer the trait's default
refusal, which is what this host always answered. `merged_head` answers the merge
`merged_at` does, with the head this host holds the change request at — a digest of its
inputs unless a scenario seeds a real commit, since nothing here saw the push. So a
consumer whose journey takes a late merge up through `onevcs` seeds the head that was
pushed (`library.rs`'s `late_merge` does), or `onevcs` rightly records no landing for a
head that does not carry the branch.

What people do on the host — merging (`merge_by_person`), deleting a merged head and
closing or retargeting what was based on it, closing unmerged, retargeting — are
`Host` methods rather than trait methods, and write nothing to the ledger: nobody on
this host made a call. The verdict a standing carries mirrors `onevcs`'s private
`publish::checks_verdict`; `each_verdict_reads_as_the_publication_watch_reads_the_same_rollup`
in `onevcs`'s e2e suite reads the same rollups through both and fails on any difference.

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
`onevcs` exposes no writer. The envelope written there is `onevcs`'s own — its
source word, its phases and its labels, named through it rather than copied, over
the bus core's generic envelope. The `significance` label is the one word a provider
stamps from a table rather than from `onevcs`: `src/significance.rs` mirrors the
producer's private table, and `tests/contract.rs` next door compiles both files and
holds them equal for every kind. **In-memory means the provider's state, never the stream**: both
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
