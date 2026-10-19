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
