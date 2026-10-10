# The exposure audit crate

Instructions that are true of `crates/onevcs-exposure-audit` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

This crate is a maintenance command of the workspace, not a deliverable: it is
`publish = false`, no release builds or packages it, and nothing outside this
repository may start depending on it. It lands on `main` so the audit can be
re-run once cleanup of what it found has landed, and say whether anything is left.

## It has no term logic of its own

The terms and the matcher are `onevcs::boundary`'s — `TermSource::from_committed`,
`derive_terms`, `TermMatcher` — the same ones the publication check refuses a write
with. `src/terms.rs` only builds their inputs and remembers which private identity
and which class each rule came from. A word-boundary rule, a case fold, a generic
word, or a manifest reader added here is a second definition of a term that can
drift from the check's; change `crates/onevcs/src/boundary/` instead, and the audit
follows.

## Its output is the boundary it audits

- **Nothing private reaches public output.** stdout, stderr, the coverage manifest
  and `measurements.json` carry numbers, the words `src/status.rs` defines, the
  public repositories the run confirmed, and the vault's own opaque path. A
  repository name, a term, a finding, a URL, or the text of a host or git error goes
  to the vault or nowhere. A new line of output is added to the journeys' checks
  for private values in the same change.
- **The vault stays outside every checkout**, mode `0700` with `0600` files. Every
  clone — the public mirrors it scans and the private `HEAD` clones terms are
  derived from — lives inside the vault, never beside the checkout the run started
  from.
- **The audit only reads.** `GET` and GraphQL queries only, and clones. A write of
  any kind — to a repository, an issue, a board, or onevcs's registry — is out of
  its scope, and the journeys assert every request it makes is a read and no
  remote ref moved.
- **A surface it could not read is a gap, never clean.** Its status says why in the
  fixed vocabulary; it is never left out or reported `scanned`. An identity whose
  committed declarations could not be read is derived from its name alone and
  listed under `source_gaps`.

## Everything committed is synthetic

Every owner, repository, term and finding in the tests and in prose here is
invented (`hiddenco`, `quietharbor`, `sample-owner`, …). A real private name, term,
finding or coverage manifest never goes into a fixture, a test, a commit message,
or a document, because each of those is public. A manifest a run writes is run
output: it stays where `--manifest-out` put it and is not committed.
