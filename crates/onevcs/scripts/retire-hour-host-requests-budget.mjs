#!/usr/bin/env node
// Report the GitHub requests one hour of finished-branch retirement spends, from the
// recorded telemetry; no onevcs, git, stand-in or timer here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readRetireHourRecord } from "./retire-hour-host-requests-budget-record.mjs";

if (process.argv.length > 2) {
  console.error(`retirement hour telemetry: expected no arguments, got ${process.argv.slice(2).join(" ")}\n` +
    "next: run it as 'node scripts/retire-hour-host-requests-budget.mjs' from crates/onevcs, as 'just budgets' does");
  process.exit(2);
}
try {
  const record = readRetireHourRecord();
  const sum = (runs, field) => runs.reduce((total, run) => total + run[field], 0);
  const passes = record.runs.filter(run => run.kind === "pass");
  const sweeps = record.runs.filter(run => run.kind === "sweep");
  const each = (runs, name) => runs.map((run, index) =>
    `${name} ${index + 1}: ${run.requests} requests (${run.merged_views} merged-state views, ${run.open_lists} open lists, ` +
    `${run.merge_times} merge times, ${run.other} other), ${run.examined} branches examined, ${run.asked} asked about, ` +
    `${run.from_records} decided from records alone, ${run.retired} retired`).join("; ");
  const detail = `invocation ${record.build.run_id}: ${record.identities} identities, ${passes.length} passes and ${sweeps.length} sweeps; ` +
    `by kind over the hour: ${sum(record.runs, "merged_views")} merged-state views, ${sum(record.runs, "open_lists")} open lists, ` +
    `${sum(record.runs, "merge_times")} merge times, ${sum(record.runs, "other")} other; ` +
    `${each(passes, "pass")}; ${each(sweeps, "sweep")}; at load1 ${record.load1}`;
  if (!report(record.requests, detail)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`retirement hour telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
