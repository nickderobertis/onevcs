# onevcs-testing

Test implementations of the two interfaces [`onevcs`](https://crates.io/crates/onevcs)
is built around, so a consumer can drive a real `onevcs` through a real journey
without a real GitHub.

```toml
[dev-dependencies]
onevcs-testing = "0.1"
```

A separate crate rather than a feature on `onevcs`, deliberately: Cargo features
are additive across a dependency graph, so a feature could be switched on by
somebody else's dependency and end up in a release binary. A `dev-dependencies`
entry cannot.

## The four providers

|                              | State             | Reach for it when                              |
| ---------------------------- | ----------------- | ---------------------------------------------- |
| `MemoryVcs` / `MemoryHost`   | this process      | one invocation, no filesystem, fastest         |
| `FileVcs` / `FileHost`       | one JSON document | several invocations that must see one another  |

Each is constructible empty and seeded from a `VcsState` or a `HostState`, and
each hands its state back. Both flavours of an interface are one implementation
over a different store, so a behaviour cannot exist in one and not the other.

```rust
use onevcs::{cli::Cli, run_with, Providers};
use onevcs_testing::{MemoryHost, MemoryVcs};

let vcs = MemoryVcs::seeded(scenario);
let host = MemoryHost::new();
let code = run_with(&cli, Providers { vcs: &vcs, hosting: &host });

assert_eq!(code, 0);
assert_eq!(vcs.state().sessions.len(), 1);
```

## What they are honest about

They emit the events the real implementations emit, into the same `$ONEVCS_HOME`
stream the real ones write — so `onevcs events TOKEN` and `onevcs artifact cat ID`
read a provider's run exactly as they read a real one. That is checked rather than
claimed: one publication journey runs twice in this repository's suite, once on
`Git` + `GitHub` and once on these, and the two event streams are held to each
other.

What they are **not** is git and GitHub. Nothing here clones, commits, pushes, or
moves a ref, and a merge records an outcome rather than advancing an origin. A
journey about git drives the real `Git`.

## License

MIT.
