# Recorded GitHub GraphQL responses for `follow_checks` and `release_holdup`

Nothing here is hand-written. Each `<name>.json` is exactly what `gh api graphql`
printed for GitHub's answer, and `<name>.args` is the argument vector the `onevcs`
binary handed `gh` for it, one argument a line (`printf '%s\n' "$@"`).

## How they were recorded

On 2026-11-02, with the user's own credential, read-only:

- `printobserver-*` — the real binary was run against the public
  `nickderobertis/printobserver` repository with `ONEVCS_GH` naming a wrapper that ran
  the real `gh` and kept each argument vector and answer. `onevcs release holdup`
  (a `release_change: {head: "release-plz-*"}` rule) made the listing and the one
  standings read of its open release-plz pull request, #298;
  `onevcs change follow-checks` over the repository's 51 newest pull requests, each at
  its head, made the two standings queries of 50 and 1.
- `vscode-standings-rulesets` — the standings query with declarations, as the binary
  builds it (the holdup's second query with only the pull request number changed), sent
  for `microsoft/vscode`'s #345592. `printobserver` declares no ruleset, and this answer
  carries rulesets whose rules have `null`, `{}` and `requiredStatusChecks` parameters.
- `printobserver-standings-without-closes` — the holdup's standings query with its
  `closingIssuesReferences` selection taken out: the query a read asks again where a
  credential may not see the issues a change request closes.
- `pydantic-standings-status-context` — the standings query with declarations sent for
  `pydantic/pydantic`'s #14022, whose head carries a commit status: the one answer here
  with a `StatusContext` node. (A pull request from a fork is no use for this: GitHub
  resolves its `headRef` in the fork and answers `isRequired(pullRequestNumber:)` there
  `NOT_FOUND`.)
- `introspection.json` — GitHub's own introspection of each type those queries select
  (`__type(name:)` with every field's type), one request a type, keyed by type name. It
  is what says which fields GitHub's schema lets be `null`.

`onevcs boundary check --destination public` passes every file here.

## Who reads them

- `tests/e2e/graphql_replay.rs` serves the `printobserver` recordings byte for byte, each
  only for the argument vector it was recorded with, so the journeys there fail when
  the queries this build sends stop being the ones GitHub answered.
- `tests/e2e/recorded_shape.rs` holds every answer the substituted host in
  `standings_host.rs` serves to these queries to the recordings' shape: every object
  carries exactly the keys GitHub's answer to the same selection carried, and a value is
  `null` only where `introspection.json` says the field is nullable.

## Re-recording

A change to either query changes its text, and the replay then refuses it. Record the
new answer the same way — the real binary through a recording `ONEVCS_GH` against a
public repository — replace the pair, and run the boundary check over it before
committing.
