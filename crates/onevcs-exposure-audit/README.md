# onevcs-exposure-audit

A read-only audit of what an owner's public repositories and boards already expose
about private ones. It is an unpublished workspace crate of onevcs: run it after
cleanup lands to see whether anything is still exposed.

`cargo run --release -p onevcs-exposure-audit -- run --help` is the reference for
its flags and exit statuses; this file says what it does and where its output goes.

- **Scope.** The set is derived on every run from the owner's public listing, and
  each repository's visibility is re-read before it is read. Private identities come
  from onevcs's registry and the account's private repositories.
- **Terms.** Each private repository is cloned at its `HEAD` commit, and its terms
  are derived by `onevcs::boundary` from what it has committed — package names and
  its `private-terms.toml` — then matched by the same matcher the publication check
  uses. The audit has no matching or derivation of its own, so a row it reports is a
  text that check would refuse.
- **Surfaces.** Every ref of each public repository, current files and full
  history; issues, change requests, reviews and their edit and title histories; and
  the boards named with `--board`.
- **Output.** Findings, each naming its term, class, match mode and the private
  identities it came from, go only to a vault directory (mode `0700`, files `0600`)
  outside every checkout. What it prints, and the coverage manifest it writes with
  `--manifest-out`, carry coverage in the words `src/status.rs` defines and no
  finding.

The journeys in `tests/journeys.rs` are the executable statement of all of it: they
drive the binary against seeded git remotes and a loopback GitHub, every name
synthetic, and say what each output may and may not contain.
