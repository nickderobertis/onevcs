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
```

A branch that outlived the session that cut it is landed by name instead, under
that same rules-resolved policy: `onevcs publish-branch feature/thing --repo
~/projects/widgets` for work that finished, and `onevcs recover` for a step that
stopped half way, which publishes it with the attestation that a green gate
cleared it. Whichever of the three refuses a branch names the one that takes it.

`onevcs status REF` answers what became of a piece of work, asked by whichever
name you hold — a change request's URL, a session token, a branch, or a commit. It
reports the identity's resolved policy, the session, every checkout and per-run
clone holding the branch, whether the change **landed** and what says so, the
host's checks, the last gate verdict, and the command that advances it. Landing is
decided from the base's own history — a recorded landing, the change request's
number in the base's log, or a landing trailer — so a change that squash-merged
reads as landed rather than as unpublished however far the base has moved since,
and one that history cannot decide reads as `unknown` rather than as work nobody
published. A host that cannot be reached leaves its section unavailable instead of
failing the command.

`onevcs import BRANCH --repo PATH [--from SOURCE] [--as NAME]` makes a branch
reachable from an identity's registered checkouts, so a later run's clone can see
work a stopped run left in its own. It writes refs and nothing else — no checkout,
no working tree — and refuses a non-fast-forward overwrite by naming the commits
it would lose.

`onevcs sweep [--dry-run] [--min-age-hours HOURS]` reclaims the workspaces those
landings leave behind. Every branch published by name cuts a run root — a clone, a
worktree, and the gate's preserved logs — under the state root, and until this verb
existed nothing removed one; thirty-one of them filled a host's disk twice in a
single run. It removes a workspace only where this tool can *prove* it is finished:
its gate recorded a verdict, no live session holds its occupancy lease, it was last
written outside the age floor `--min-age-hours` sets, and removing it is something
this host can do at all. Everything else is retained and reported with the reason —
a workspace somebody is still publishing in is never removed and never terminated,
and neither is one belonging to another manager on a shared state root — and the
per-run lifecycle clones a session keeps as recovery history are outside the verb
entirely.

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
gate that failed, a request that was invalid, and a base that moved under it.

## Embed it

A command answers a process: an exit code and a line of prose. A caller embedding
the crate wants the decision, so the same operations answer values.

```rust,ignore
let published = onevcs::publish(&providers, &token, &PublishRequest::default())?;
match published.outcome {
    PublishOutcome::Merged(sha) => journal.landed(sha),
    PublishOutcome::ChangeOpen(url) | PublishOutcome::Queued(url) => journal.awaiting(url),
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
absent) and no `exclude` matcher, matching `source` by family, `kind` by glob
(`change-*`), and the reserved label keys exactly:

```yaml
include:
  - {source: vcs, kind: "gate-*"}
exclude:
  - {kind: lock-wait}
```

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
