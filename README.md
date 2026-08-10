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

Everything durable lives under one state root — `ONEVCS_HOME`, otherwise
`~/.onevcs`.

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

## Test against it, without a real GitHub

Embedding the crate, `run_with(&cli, providers)` takes the two implementations a
run reaches `Vcs` and `RemoteHost` through; `run` is that with `Git` and GitHub.
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
