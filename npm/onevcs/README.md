# onevcs-cli

Version control and its remote host behind one host-neutral vocabulary: the
review unit is a **change request**, and a rules file decides how each repository
publishes one.

```console
npm install -g onevcs-cli
onevcs --help
```

Or without installing:

```console
npx onevcs-cli --help
```

This package ships the **prebuilt** `onevcs` binary: installing it needs no Rust
toolchain and compiles nothing. The binary for your platform arrives through an
optional dependency (`onevcs-cli-<platform>-<arch>`), so npm downloads exactly
one of them.

Prebuilt binaries exist for Linux (x64, arm64), macOS (x64, arm64), and Windows
(x64). On any other platform, install with `cargo install onevcs`.

> **Interface-only.** The command surface and the contract behind it are final;
> the behaviour is not implemented yet, so every subcommand refuses with exit
> code 70.

<!-- llmlint: ignore[no_redundant_instruction_pointers] this README is rendered on
npmjs.com, where the repository is not otherwise in front of the reader — the pointer is
the only path from the installed package back to its documentation. -->
Full documentation: <https://github.com/nickderobertis/onevcs>
