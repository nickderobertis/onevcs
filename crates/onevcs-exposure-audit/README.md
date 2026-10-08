# onevcs-exposure-audit

A measurement spike's command, kept on its preserved branch and never published.
It answers two questions before a repository publication boundary is designed:
what an owner's public repositories and boards already expose about private ones,
and what a boundary check over them would cost.

## `run`: the read-only audit

```sh
cargo run --release -p onevcs-exposure-audit -- run \
  --owner OWNER --pushed-since YYYY-MM-DD \
  --board OWNER/NUMBER --board-issues OWNER/NAME \
  --projects-token-env NAME --expected-count N \
  --manifest-out PATH
```

- **The set is derived on every run**, never listed: every repository the owner
  lists as public, forks included, last pushed on or after the cutoff, each one's
  visibility then re-read and dropped unless still public. `--allow` narrows it;
  `--registered-only` swaps it for the identities onevcs has registered here that
  are public, for comparison. Repositories are read through temporary mirror clones
  inside the vault and the GitHub API, and none is registered.
- **Terms come from private identities**: the registered ones that are not public
  (unknown and local-only count as private) and every private repository the
  credential can list. Each yields `owner/name`, its bare name, its owner, and the
  package names in its root manifests. Bare names that are generic words, that are
  also a public repository's name, or owners shared with the public set are matched
  for the false-positive survey only. `--exceptions` declares more.
- **Findings go only to the vault**: `${XDG_STATE_HOME:-~/.local/state}/ai-orchestrator/private-boundary-audit/<opaque id>/`,
  mode `0700`, files `0600`, refused inside any git checkout. `report.md` splits
  them into current files, git history by repository, commit and term, and
  issues, change requests and board items with whether an edit would leave the old
  text visible.
- **Public output is coverage only**: stdout carries numbers and fixed words, and
  `--manifest-out` writes one row per confirmed-public repository and board, each
  surface one of `scanned`, `not-found`, `permission-denied`, `rate-limited`,
  `other-error`. No finding, term, count of either, or error text.
- **Read-only**: `GET` and GraphQL queries only; after a quota refusal no further
  request is made and every later surface reads `rate-limited`.

The credential is `GH_TOKEN` (`--token-env`), else `gh auth token`. Boards need
`read:project`, which `--projects-token-env` supplies; without it they are
recorded `permission-denied`, never clean.

## `bench`: the generated envelopes

```sh
cargo run --release -p onevcs-exposure-audit -- bench --scale 1
cargo run --release -p onevcs-exposure-audit -- bench --scale 10
```

One JSON document per scale: matcher build time, in-memory check latency, an
export of committed files into one fresh commit, the publication check over the
diff git produces, the visibility-read arithmetic, and peak memory. The defaults
are the plan's upper bound: 50 identities, 20 private repositories of 100 terms,
10 MiB of changed text over 1000 paths, 100 tasks per run.
