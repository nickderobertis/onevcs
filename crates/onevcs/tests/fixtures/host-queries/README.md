# Recorded GitHub responses for `onevcs host`

`tests/e2e/host_queries.rs` replays these through `ONEVCS_GH`. Nothing here is
hand-written: every file under `recordings/` is a real GitHub REST response.

## Format

Each recording is one JSON envelope, `{"request": {"method", "url"}, "status",
"headers", "body"}`, with `body` the parsed response and `headers` limited to
`content-type`, `date`, `etag`, `last-modified`, `link` and the `x-github-api-version-selected`
and `x-ratelimit-*` headers. Nothing that carries a credential was kept.

The replay serves a recording's status, its headers (canonically capitalised, `Link`
included) and its body, re-serialised without whitespace in its recorded key order, the way
`gh api --include` prints a response.

## Where each set came from

- `onepipeline-*` directories — recorded by the `spike-actions-telemetry` spike from the
  public `nickderobertis/onepipeline` repository: one CI run, a run re-run to attempt 3, the
  v0.72.0 release with its pull requests' runs and jobs, and a 71-job listing at 30 a page.
  Only the recordings a journey reads were copied.
- `recorded-for-host-queries/` — recorded for this suite, read-only, with `gh api --include
  --method GET`: onepipeline's closed-pull-request page carrying v0.72.0's three pull
  requests (and the same listing at seventeen a page, whose first page ends after v0.71.1's
  commit), its releases listing at five a page (so the previous release is on page 2), an
  empty `pull_request` listing at the release commit, the jobs of the Visual docs run #914's
  head ran again just after its merge, and GitHub's 404 and 401 answers;
  `skilltest`'s v0.13.0 range, whose last commit was pushed with no pull request; and a
  `nick-derobertis-site` pull request head whose Visual docs run failed to start, the only
  run with an empty job listing in the public repositories surveyed.

## Scrubbing

The public boundary refused one name in a few recordings: a private organisation's domain,
in commit authors' e-mail addresses and commit messages. It was replaced by `example`,
in the text of the file and nowhere else. No field an answer reports carries it, and the
spike's `hand_check.sh` reaches the same selections from the scrubbed files as from the
originals. `onevcs boundary check --destination public` passes every file here.

## Scenarios

`scenarios/*.json` say what one replay serves: each entry names a recording, served for the
request it recorded unless `for` names another, and `keep_runs` leaves every other run out
of a runs listing (with `total_count` set to match) — the only way an answer is assembled,
and each one says `why`.
