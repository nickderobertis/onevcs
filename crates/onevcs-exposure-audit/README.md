# onevcs-exposure-audit

A measurement spike's command, kept on its preserved branch and never published.
It answers two questions before a repository publication boundary is designed:
what an owner's public repositories and boards already expose about private ones,
and what a boundary check over them would cost.

`cargo run --release -p onevcs-exposure-audit -- --help` is the reference for its
two subcommands and their flags; this file says what they are for and where their
output goes, and does not repeat either.

- **`run`** is the read-only audit. It derives its set on every run from the
  owner's public listing and re-reads each repository's visibility before reading
  it, and derives its terms from private identities. The full findings go only to
  the host-local vault, outside every checkout; what it prints, and the coverage
  manifest it can write, carry coverage in the words `src/status.rs` defines and no
  finding. `coverage-manifest.md` is that manifest for the run over the approved
  scope, and a unit test holds it to the renderer.
- **`bench`** measures the matcher, an export, and a publication check on generated
  data at a workload and its multiples, and prints one JSON document per scale.

The journeys in `tests/journeys.rs` are the executable statement of both: they
drive the binary against seeded git remotes and a loopback GitHub, and say what
each output may and may not contain.
