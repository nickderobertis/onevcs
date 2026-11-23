#!/usr/bin/env node
// Report validated measurements; no subprocess, fixture preparation or timing here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readRecord } from "./recoverable-budget-record.mjs";

try {
  const args = process.argv.slice(2);
  const record = readRecord();
  let value;
  let description;
  if (args.length === 1 && args[0] === "--journey-time") {
    value = record.total_ms / 1000;
    const holders = record.holders;
    const walls = holders.warm.map(call => call.wall_ms).sort((a, b) => a - b);
    const median = (walls[4] + walls[5]) / 2;
    const ms = n => `${n.toFixed(1)}ms`;
    description = `empty-root preparation ${record.preparation_ms / 1000}s; complete journey at load1 ${record.load1}; ` +
      `holder read over ${holders.records} records: cold ${ms(holders.cold.wall_ms)}, warm median ${ms(median)}, ` +
      `slowest warm ${ms(walls[walls.length - 1])}, further warm call ${holders.git_executions} Git`;
  } else if (args.length === 3 && args[0] === "--scale" && ["1", "10"].includes(args[1]) && ["--cold", "--warm"].includes(args[2])) {
    const workload = record.workloads.find(row => row.scale === Number(args[1]));
    const mode = args[2].slice(2);
    value = workload[`${mode}_git`];
    description = `${mode} launcher Decision, scale ${workload.scale}, separate actual Git executable count`;
  } else if (args.length === 3 && args[0] === "--scale" && args[1] === "host" && args[2] === "--warm") {
    // The unfiltered read over the aged host-shaped fixture, from the state each timed
    // call starts from: every git it starts, whichever path in the read starts it.
    value = record.host.git_executions;
    description = `host warm unfiltered Decision over the aged host-shaped fixture, separate actual Git executable count`;
  } else {
    throw new Error("expected --scale 1|10 --cold|--warm, --scale host --warm, or --journey-time");
  }
  if (!report(value, `invocation ${record.build.run_id}: ${description}`)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`recoverable telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
