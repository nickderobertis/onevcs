# The exposure audit crate

Instructions that are true of `crates/onevcs-exposure-audit` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

This crate is a measurement spike's command. It lives on the spike's preserved
branch, is `publish = false`, and never lands, so nothing outside the spike may
start depending on it.

## Its output is the boundary it measures

- **Nothing private reaches public output.** stdout, stderr, the coverage manifest
  and `measurements.json` carry numbers, the words `src/status.rs` defines, the
  public repositories the run confirmed, and the vault's own opaque path. A
  repository name, a term, a finding, a URL, or the text of a host or git error goes
  to the vault or nowhere. A new line of output is added to the journeys' checks
  for private values in the same change.
- **The vault stays outside every checkout**, mode `0700` with `0600` files. A
  temporary clone lives inside the vault, never beside the checkout the run started
  from.
- **The audit only reads.** `GET` and GraphQL queries only. A write of any kind —
  to a repository, an issue, a board, or onevcs's registry — is out of its scope,
  and the journeys assert every request it makes is a read.
- **A surface it could not read is a gap, never clean.** Its status says why in the
  fixed vocabulary; it is never left out or reported `scanned`.

## Its fixtures are synthetic

Every owner, repository, term and finding in the tests and in prose here is
invented. A real finding, term or private name never goes into a fixture, a test, a
commit message, or a document, because each of those is public.

`coverage-manifest.md` is the audit's own output over the approved scope; re-run
the audit to change it rather than editing it, since a unit test holds it to the
renderer.
