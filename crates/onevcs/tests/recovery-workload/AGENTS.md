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

`boundary.rs` is the public boundary's budget journey: it builds
`onevcs_testing::boundary`'s workload, drives the release binary's `publish-branch`
and `export` over it, and writes `boundary.json` beside `recoverable.json`, stamped
with the same invocation, binary and source identity — the wrapper retracts both
together. The check's phases come from the binary's own `ONEVCS_BOUNDARY_DIAGNOSTICS`
line and its peak memory from `wait4`; `scripts/boundary-check-budget.mjs` is the only
reader, and `crates/onevcs/budgets.yaml` the only budget over it. It takes
`exclusive()` too, so neither workload's clock carries the other's load.
