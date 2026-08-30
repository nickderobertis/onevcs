# onevcs

Version control and its remote host behind one host-neutral vocabulary, for agent
workflows.

The review unit is a **change request** — GitHub maps it to a pull request, and a
later host maps it to whatever it calls the same thing. `Vcs` owns the repository
side (identities, sessions over an isolated worktree, preserved work);
`RemoteHost` owns the host side (opening a change, reading its checks, merging
it); a **rules file** decides, per repository, how a change publishes and what
verifies it. Everything a run does is emitted as an NDJSON event stream.

The public surface is the approved contract — [`docs/contract.md`](docs/contract.md)
— compiled, and it is implemented: the registry with its lazy migration, the rules
engine, sessions over borrowing clones, bounded git, the FIFO merge queue, both
publication strategies, recovery and provenance, the merge train, and the event
stream.

## What one change looks like

```console
onevcs register ~/projects/widgets                 # once per checkout
onevcs rules check widgets                         # which policy, and why
token=$(onevcs session open widgets --branch feature/thing | jq -r .token)
# …work in the worktree the session printed…
onevcs publish "$token"                            # verify, then land it
onevcs events "$token"                             # everything it did, as NDJSON
onevcs release status "$token"                     # …and whether a release carries it
```

A branch that outlived the session that cut it is landed by name instead, under
that same rules-resolved policy: `onevcs publish-branch feature/thing --repo
~/projects/widgets` for work that finished, and `onevcs recover` for a step that
stopped half way, which publishes it with the attestation that verification
cleared it. Whichever of the three refuses a branch names the one that takes it.

`onevcs status REF` answers what became of a piece of work, asked by whichever
name you hold — a change request's URL, a session token, a branch, or a commit. It
reports the identity's resolved policy, the session, every checkout and per-run
clone holding the branch, whether the change **landed** and what says so, the
host's checks, the last thing its merge path said about it, and the command that
advances it. Landing is
decided from the base's own history — a recorded landing, the change request's
number in the base's log, or a landing trailer — so a change that squash-merged
reads as landed rather than as unpublished however far the base has moved since,
and one that history cannot decide reads as `unknown` rather than as work nobody
published. A host that cannot be reached leaves its section unavailable instead of
failing the command.

A branch is often worked on by more than one session — a run stops and the next
one continues the name — so the older session's record names the one that
continued it, and every answer about a session or a branch follows that chain to
its newest record before it reports a landing. A copy of the branch that was
superseded is still reported as holding it and no longer decides anything about
it. A chain this host cannot follow — a session record removed underneath one, a
link into another repository, a cycle — reports `unknown` and says why, rather
than falling back to whichever record still read: a wrong `no` there reads as an
instruction to publish work the base already carries.

`onevcs release` answers what happens **after** a change lands, so an upgrade can
be sequenced behind the release that carries it rather than behind the merge.
`onevcs release targets REPO` lists what a repository releases and whether it
adopts `fast` (the work is enough) or `published` (the release is what is depended
on); `onevcs release latest REPO [--target NAME]` says what is out right now; and
`onevcs release status REF [--target NAME]` says whether the release carrying one
landed change has happened yet, asked by the same four names `onevcs status` takes.

A target's **style decides its shape**. An *automated* target carries a probe — a
script the repository carries, or a one-liner run through `sh` — and is answered by
running it under a bounded timeout; a *human-step* target carries no probe at all,
because the release happens when a person does something, and is answered by
`onevcs release acknowledge REF --target NAME --version VERSION` after they have
done it. Recording the same version again is a no-op; a different one is refused
until `--supersede` replaces it, which keeps the version it replaced.

Two answers stay apart everywhere, and a consumer routes on the difference: **"not
released" is a probe that answered**, and **"not answered" is a probe that did
not** — a timeout, a non-zero exit, or output that is not one usable version. A
landing whose probe never answered is "not answered" for ever rather than being
compared against a reading taken later, because the release carrying that very
change may already be in it. Configure targets in `$ONEVCS_HOME/releases.yml`; a
host with none behaves exactly as it did before there was one.

`onevcs import BRANCH --repo PATH [--from SOURCE] [--as NAME]` makes a branch
reachable from an identity's registered checkouts, so a later run's clone can see
work a stopped run left in its own. It writes refs and nothing else — no checkout,
no working tree — and refuses a non-fast-forward overwrite by naming the commits
it would lose.

`onevcs sweep [--dry-run] [--min-age-hours HOURS]` reclaims the workspaces those
landings leave behind. Every branch published by name cuts a run root — a clone, a
worktree, and the merge path's preserved logs — under the state root, and until this rule
existed nothing removed one; thirty-one of them filled a host's disk twice in a
single run. **The rule runs whether or not anybody asks:** each landing enforces it
over its own family before cutting the next run root, and this verb is the same
judgement asked deliberately and over every family at once.

A workspace is removed only where this tool can *prove* it is finished: its merge
path recorded a verdict, no live session holds its occupancy lease, it was last written
outside the age floor `--min-age-hours` sets, and removing it is something this host
can do at all. Everything else is retained and reported with the reason — a
workspace somebody is still publishing in is never removed, and neither is one
belonging to another manager on a shared state root — and the per-run lifecycle
clones a session keeps as recovery history are outside it entirely.

Two things a proven-finished workspace still gets to keep. Its preserved logs
last at least as long as the age floor, because they are what an operator reads
after a publication failed; and a clone still holding work that never reached the
origin is kept past the floor too, until enough newer ones stand in front of it —
under the same bound the per-run lifecycle clones a session keeps are kept under.
Reclaiming a workspace also **stops the processes that publication left running** —
a publication runs the repository's own verification and verifications start
daemons, and
unlinking files a live process holds open frees none of their blocks.

Everything durable lives under one state root — `ONEVCS_HOME`, otherwise
`~/.onevcs`.

GitHub is reached through `gh`, so whatever `gh auth status` reports is the
credential. A **fine-grained personal access token needs `Actions: Read`** on the
repository beside its contents and pull-requests access: GitHub will not let that
credential class resolve a check run at all — there is no `Checks` permission to
grant one — so a change request's checks are read from its workflow runs, and
`change_checks` says so in the `sources` it answers with. Anything a third-party
integration posted as a check run or a commit status is invisible to such a token,
and a credential that can read neither source is refused rather than reported as
having no checks.

## Install

```console
cargo install onevcs      # crates.io
pip install onevcs-cli    # a prebuilt wheel, no Rust toolchain
npm install -g onevcs-cli # a prebuilt binary, no Rust toolchain
```

All three install the same `onevcs` binary. Prebuilt binaries exist for Linux
(x64, arm64), macOS (x64, arm64), and Windows (x64); every release also attaches
the archives and their `.sha256` checksums for a direct download.

## Use

```console
onevcs --help
```

`--help` is the command surface, and `publish` reserves its own exit codes for a
verification that failed, a request that was invalid, and a base that moved under
it.

## Embed it

A command answers a process: an exit code and a line of prose. A caller embedding
the crate wants the decision, so the same operations answer values.

```rust,ignore
let published = onevcs::publish(&providers, &token, &PublishRequest::default())?;
match published.outcome {
    PublishOutcome::Merged(sha) => journal.landed(sha),
    PublishOutcome::ChangeOpen(url) | PublishOutcome::Queued(url) => journal.awaiting(url),
    // A draft cannot land while it stands; publishing again with no `draft` lifts it.
    PublishOutcome::ChangeDraft(url) => journal.held_back(url),
    PublishOutcome::NothingToPublish => journal.nothing(),
    PublishOutcome::Failed { kind, reason, retained } => journal.failed(kind, reason, retained),
}
```

`close_session` and `session` are the same for the rest of a session's life, and
`EventStream::open(&token)` reads its events as `Envelope`s, each attributed to
the session that wrote it — so a caller following several publications at once can
tell them apart. The command line is a rendering of these rather than a second
path through them.

A consumer that wants some of what a session writes reads it through the filter
grammar the three producing libraries share — `EventStream::open_filtered(&token,
filter)`, or `onevcs events TOKEN --filter SPEC` with the spec inline as JSON or in
a file. An envelope passes when it matches any `include` matcher (or `include` is
absent) and no `exclude` matcher, matching `source` by family, `phase` and the
reserved label keys exactly, and `kind` by glob (`change-*`):

```yaml
include:
  - {source: vcs, kind: "gate-*"}
exclude:
  - {kind: lock-wait}
```

Every envelope carries the **phase** of a change's life its producer stamped it
with — `development` (the work being made, including a push of the session's own
branch), `integrate` (the merge queue, the merge, a sync conflict, and a push of
any other branch), `review` (the change request opened, checked, and merged), and
`release` (a probe, an acknowledgement, an observation). Naming a phase is how a
consumer asks for "the review of this change" without listing the kinds in it, so
a kind added to that phase later arrives in the read that already wanted it.

`EventStream` takes the phases the session can actually produce: `development` and
`integrate` always, `review` only where the resolved merge policy is not
`local-direct`, and `release` only where `$ONEVCS_HOME/releases.yml` configures
targets for that repository. Naming a phase a session does not have is refused by
name, because a filter answered with silence and a session that did nothing look
alike; naming none takes what there is. And where `release` is one of them, the
read **also** hands back that repository's `release-observed` and
`release-acknowledged` events whose landing commit is this session's own — the
correlation `onevcs` can already make, so a consumer never has to find the
repository's release stream. That set grows after the session closes, because a
release happens when it happens.

Before a caller has a token, `session_holders(repo)` answers who is in a
repository: one `SessionHolder` per recorded session, carrying the token the calls
above take, the branch and worktree it holds, and whether its owner is still
running (`Liveness::Live`) or the session is the remains of a run that stopped.

## Test against it, without a real GitHub

Embedding the crate, `run_with(&cli, providers)` takes the two implementations a
run reaches `Vcs` and `RemoteHost` through; `run` is that with `Git` and GitHub,
and every entry point above goes through the same seam.
[`onevcs-testing`](crates/onevcs-testing) ships in-memory and file-backed
implementations of both, so a consumer's suite drives a real `onevcs` through a
real journey against a host it seeded:

```console
cargo add --dev onevcs-testing
```

They emit the events the real implementations emit — a claim this repository's
own suite checks by running one publication journey on both backends and holding
the two event streams to each other.

## Develop

```console
just bootstrap   # from a clean clone
just check       # the deterministic gate: format, clippy, tests, coverage, docs
just gate        # check, plus the diff-scoped LLM-judge tier — the pre-push bar
```

`just --list` is the full index.

## License

MIT. See [`LICENSE`](LICENSE).
