# What three change-request journeys asked the host before closures were reported

Each `.log` is the `gh-calls.log` the substituted `gh` (`../gh`) wrote for one journey,
recorded on `90e1833` — the tree before the landing events reported the issues they
close — and committed unchanged. `tests/e2e/call_baseline.rs` holds the same journey on
the current tree to it: no invocation occurs more times than it does here, counting a
read that only gained the `closingIssuesReferences` field (or that connection's selection
set in a GraphQL query) as the same invocation.

| Baseline | Journey |
| --- | --- |
| `watched-merge.log` | `edges::a_change_merged_directly_lands_without_waiting_for_the_host_to_hold_it`; and `edges::a_read_refused_over_the_closures_field_alone_still_lands_and_reports_none`, the same journey with the closures refused, held to it **plus exactly one** merge read (`assert_one_call_beyond`) — the one request the manager's ruling allows |
| `late-merge-status.log` | `library::a_change_merged_after_its_session_closed_is_recorded_by_the_next_status_with_the_issues_it_closes` |
| `late-merge-standings.log` | `library::a_change_merged_after_its_session_closed_is_recorded_by_the_standings_read_with_the_issues_it_closes` |
| `history-landing-release-status.log` | `landing::a_landing_found_only_in_the_bases_history_reports_the_issues_its_commit_closes` — held **exactly** (`assert_calls_unchanged`): that path must ask the host what it asked before, no less |

They were made by running those journeys on `90e1833`'s `src/` with a one-off copy of the
world's `gh-state/gh-calls.log` at the point each journey now checks it; that copy is not
part of the tree. Re-record one only deliberately, on behaviour that has not moved, the
same way — never to absorb a request a change added.

`late-merge-status.log` carries two lines `90e1833` did not ask: `repo view … --json
autoMergeAllowed` and `pr view 1 … --json isDraft`. They are the publication's, not the
closures read's: a `change-auto` publication stopped arming the host's merge while its
checks were still pending, so this journey's elapsed wait now arms it the way every
other elapsed wait always did — asking whether the repository allows auto-merge, and
lifting any draft, first. Added deliberately with that change, and nothing else moved.

`watched-merge.log` carries one line `90e1833` did not ask: a second `pr view 1 … --json
headRefOid,statusCheckRollup`, after `pr ready`. It is the publication's, not the
closures read's: the checks now settle outside the identity's merge turn, so the
publication re-reads them under the turn to confirm they are still green on the head
that settled before it asks for the merge. Added deliberately with that change.

`late-merge-standings.log`'s one GraphQL query selects the check rollup's `pageInfo {
hasNextPage }`, which `90e1833` did not ask. It is the same request, not another: the
standings query gained that field so a follow or holdup read can refuse a head carrying
more checks than one read returns, and the journey still sends that one query once.
Edited deliberately with that change, and nothing else moved.

**All four were re-recorded for onevcs#229**, on the change that moved a publication's
post-push reads off GitHub's GraphQL API and onto REST — so a spent GraphQL allowance no
longer leaves a pushed change unverified — with the same journeys, by the same method,
and the closures field taken back out of each line as it was before. What moved is the
transport and nothing the closures read asks:

- `gh api user --jq .login` is `gh api user --include`, the same request.
- `gh pr list … --json …` is `gh api repos/…/pulls?head=<owner>:<branch>&base=…&state=open`.
- `gh pr view N --json headRefOid` (the head of a change request just opened) is `gh api
  repos/…/pulls/N --include`, whose draft state also answers the `isDraft` view that
  followed it — so that view is gone.
- each reading of the checks, `gh pr view N --json headRefOid,statusCheckRollup`, is three
  REST requests: the change request's head (`pulls/N`), then the check runs and the commit
  statuses on that commit (`commits/{sha}/check-runs`, `commits/{sha}/status`). What blocks
  the merge is read from the rulesets and classic protection once per publication, the
  same read the watch's declaration already made, rather than from `gh pr checks
  --required` per reading.
- a commit hash in a REST path is written `{sha}`, and `call_baseline.rs` reads every
  call that way, because a journey's commits are its own.

The merge, the draft lift, the merge-state view that carries the closures, `mergedAt`,
`repo view … autoMergeAllowed` and the standings query stay GraphQL, unchanged:
`watched-merge.log` asked 10 GraphQL requests and 4 REST ones before, and asks 5 and 12
now.
