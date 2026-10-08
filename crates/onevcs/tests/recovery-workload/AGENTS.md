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
