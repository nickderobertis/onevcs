# onevcs

Version control and its remote host behind one host-neutral vocabulary, for agent
workflows.

The review unit is a **change request** — GitHub maps it to a pull request, and a
later host maps it to whatever it calls the same thing. `Vcs` owns the repository
side (identities, sessions over an isolated worktree, preserved work);
`RemoteHost` owns the host side (opening a change, reading its checks, merging
it); a **rules file** decides, per repository, how a change publishes and what
verifies it. Everything a run does is emitted as an NDJSON event stream.

> ## Interface-only, for now
>
> This repository is the approved contract — [`docs/contract.md`](docs/contract.md)
> — compiled. Every public type, trait, config schema, and CLI argument is final
> and typed; nothing behind them is implemented. Trait methods return
> `Error::NotImplemented` and every CLI subcommand exits `70`. Behaviour lands
> per-seam, and each seam brings its own real journey with it.

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

The whole command surface, and the exit codes `publish` reserves (`1` gate or
checks failed, `2` invalid, `3` sync conflict), are declared in
[`docs/contract.md`](docs/contract.md).

## Develop

```console
just bootstrap   # from a clean clone
just check       # the deterministic gate: format, clippy, tests, coverage, docs
just gate        # check, plus the diff-scoped LLM-judge tier — the pre-push bar
```

`just --list` is the full index. [`AGENTS.md`](AGENTS.md) holds the durable
instructions: what the gate enforces, how releases work, and the rules that govern
changes while the crate is interface-only.

## License

MIT. See [`LICENSE`](LICENSE).
