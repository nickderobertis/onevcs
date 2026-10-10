# review-loop spike harness

A throwaway measurement for the plan `authoring:pr-feedback-rework`, preserved on its
spike branch and never landed. It is not onevcs code: it drives `gh` and `git`
directly against a disposable repository whose name ends in `-smoke`.

    scripts/spikes/review-loop/review_loop.py --repo nickderobertis/onevcs-smoke --out DIR

(`REVIEW_LOOP_REPO` stands in for `--repo`.) One run builds the draft stack `A`, `B`
on `A`, `C`, a pushed synthetic base `S` (main + A + C) and `D` on `S`; posts a line
comment, a review summary and a conversation comment, and edits one; reads every pull
request's feedback through REST (with and without `If-None-Match`), GraphQL per pull
request and GraphQL batched over 4 and 10 drafts (and once over 150, the plan's busiest
hour), recording calls, allowance deltas, bytes and wall time;
replies with the hidden `onevcs:reply` marker and attempts a threaded reply to the
review summary; probes whether a 304 is charged; restacks by merge; opens a cold-host
`onevcs` session on `B` with a fresh `ONEVCS_HOME` and publishes it as a draft; and
finally closes every pull request and deletes every branch it pushed. Results land in
`DIR/results.json` and `DIR/summary.md`; the run records the harness commit it ran at.
