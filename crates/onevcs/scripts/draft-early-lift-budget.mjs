#!/usr/bin/env node
// Report the GitHub requests the draft hold's running-work check adds per watch, from the
// recorded telemetry; no onevcs, git, stand-in or timer here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readDraftHoldRecord } from "./draft-early-lift-budget-record.mjs";

try {
  if (process.argv.length > 2) throw new Error("expected no arguments");
  const record = readDraftHoldRecord();
  // The figure is the worst of the four workloads; the detail carries each one's watch.
  const worst = record.scenarios.reduce((most, scenario) => (scenario.activity_requests > most.activity_requests ? scenario : most));
  const each = record.scenarios.map(scenario =>
    `${scenario.shape} (${scenario.outcome}${scenario.lifted_early ? ", lifted early" : ""}): ${scenario.polls} polls, ` +
    `${(scenario.check_read_requests / scenario.polls).toFixed(1)} check-read requests a poll, ` +
    `${scenario.watch_requests} requests in the whole watch, ${scenario.activity_reads} head-activity reads ` +
    `(${scenario.reused_reads} polls answered from the reuse), ` +
    `${(scenario.activity_requests / scenario.activity_reads).toFixed(1)} requests a read, ${scenario.activity_requests} added`);
  const detail = `invocation ${record.build.run_id}: one change-auto draft watch per workload at ${record.poll_seconds}s poll, ` +
    `${record.grace_seconds}s grace, ${record.wait_seconds}s wait and ${record.reuse_seconds}s reuse, run 1/${record.scale} ` +
    `of real time; worst is ${worst.shape}; ${each.join("; ")}; at load1 ${record.load1}`;
  if (!report(worst.activity_requests, detail)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`draft hold telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
