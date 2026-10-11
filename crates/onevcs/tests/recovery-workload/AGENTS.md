# Recovery workload journeys

`onevcs-recovery:test` builds both full-size real-Git workloads once per
invocation and writes their
validated timing/counting records, invocation identity and producing binary to
`target/budget-records/`. `onevcs:budgets` depends on that edge and only reads
those artifacts through the released onebudgetspec checker. Nx restores the
whole artifact directory together; a failed producer invalidates earlier
records before compilation starts. `just recoverable-journeys` regenerates the
same evidence without unrelated journeys. Keep these full workloads on their
own edge: an ordinary e2e edit must not rebuild them.

`churn.rs` drives the same fixtures under a concurrent ref writer with transport
configuration set, held to the registered warm thresholds; it records no
telemetry. Every workload test takes `exclusive()` first, so the timed journey's
clock never carries another workload's load.

The latency budgets are the library call a Stop hook makes, timed inside a running
process: `crates/onevcs/examples/recoverable_latency.rs`, built in release by the
recovery recipes and named by `ONEVCS_RECOVERY_LATENCY`, makes ten
`recoverable_matching` calls per mode (`warm` after one priming call, `cold` with
every cache under `cache/recoverable/v1` removed before each) and the journey holds
every call's rows to an `uncached` read (a `GIT_*` override refuses proofs and
in-process reads). `scripts/recoverable-latency-budget.mjs` reports the slowest.

`oracle-decision-{1,10}.json` hold onevcs 0.43.1's launcher-filtered Decision rows
— by count and the SHA-256 of the normalized rows, so the documents stay small — and
its cold Git count over the same fixtures, recorded by running the journey with
`ONEVCS_DECISION_BASELINE_BINARY` naming a 0.43.1 binary and
`ONEVCS_BASELINE_ORACLE_DIR` naming where to write them; with the binary named, every
read is also compared live. At the smaller workload the oracle holds each other
launcher's rows too, whose unselected sessions span every class.

`scoped.rs` counts, through an inotify watch, the session and stream records a warm
launcher read opens, before and after the host grows nine times its other launchers'
sessions and streams in the same identities; Linux only.

`boundary.rs` is the public boundary's budget journey: it builds
`onevcs_testing::boundary`'s workload, drives the release binary's `publish-branch`
and `export` over it, and writes `boundary.json` beside `recoverable.json`, stamped
with the same invocation, binary and source identity — the wrapper retracts both
together. The check's phases come from the binary's own `ONEVCS_BOUNDARY_DIAGNOSTICS`
line and its peak memory from `wait4`; `scripts/boundary-check-budget.mjs` is the only
reader, and `crates/onevcs/budgets.yaml` the only budget over it. It takes
`exclusive()` too, so neither workload's clock carries the other's load.
