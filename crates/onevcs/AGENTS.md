# The `onevcs` crate

Rules that apply inside this crate and nowhere else.

## What may be `pub`

Three helper types exist here because a named item could not be written without
them: `Policy`, `GateKind`, and `Approvals`. A fourth is a contract amendment,
never a local decision.

The module layout is not part of the contract — `lib.rs` re-exports the flat
surface a consumer sees, and modules are private except `cli`, `registry`, and
`rules`, which a consumer names to reach a config schema.

## Where the contract is under-specified

Three sites here are deliberately loose, and each carries a **line-scoped
`llmlint: ignore`** naming its own reason rather than a widened rule:
`Check.status` and `Check.conclusion` are strings because the contract enumerates
no value set for either, and both schemas' `version` is accepted as written
because deciding which versions are acceptable is implementation. A fourth needs
its open question recorded first — a suppression nobody has to answer for is a
shortcut, not a decision.

## Tests

`tests/contract.rs` is the drift gate and `tests/e2e/` is the journeys. The
non-obvious contracts they hold:

- The envelope and rules **fixtures are extracted from `docs/contract.md`**, not
  copied — so editing the doc without the types, or the reverse, fails the gate.
- The event-kind list, the command names, the long flags, and the command
  inventory `scripts/smoke-published.sh` asserts are all reconciled against the
  parser, so a command added to one place and not the others fails.
- `tests/e2e/packaging.rs` drives the committed npm launcher against a real
  assembled platform package, so the shipped resolution and exit-code
  propagation are proven without a registry.

Add a journey to the suite as its seam is implemented; the stub it replaces goes
with it.
