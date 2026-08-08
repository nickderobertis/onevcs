# onevcs

Version control and its remote host behind one host-neutral vocabulary, for agent
workflows.

The review unit is a **change request** — GitHub maps it to a pull request, and a
later host maps it to whatever it calls the same thing. `Vcs` owns the repository
side (identities, sessions over an isolated worktree, preserved work);
`RemoteHost` owns the host side (opening a change, reading its checks, merging
it); a rules file decides, per repository, how a change publishes and what
verifies it. Everything a run does is emitted as an NDJSON event stream of one
`Envelope` shape.

**This crate is interface-only.** The public surface is the approved contract,
compiled: types, traits, config schemas, and the CLI argument surface are final
and typed, and nothing behind them is implemented. Every trait method returns
`Error::NotImplemented` and every CLI subcommand exits `70`. Depend on it to build
against the contract; do not depend on it to do anything yet.

```console
cargo install onevcs      # the CLI
pip install onevcs-cli    # the same binary, prebuilt
npm install -g onevcs-cli # likewise
```

<!-- llmlint: ignore[no_redundant_instruction_pointers] this README is rendered on
crates.io and docs.rs, where the repository is not otherwise in front of the reader — the
pointer is the only path from the published crate back to its contract. -->
The contract, the command surface, and the development workflow:
<https://github.com/nickderobertis/onevcs>
