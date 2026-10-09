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
    description = `empty-root preparation ${record.preparation_ms / 1000}s; complete journey at load1 ${record.load1}`;
  } else if (args.length === 3 && args[0] === "--scale" && ["1", "10"].includes(args[1]) && ["--cold", "--warm"].includes(args[2])) {
    const workload = record.workloads.find(row => row.scale === Number(args[1]));
    const mode = args[2].slice(2);
    value = workload[`${mode}_git`];
    description = `${mode} launcher Decision, scale ${workload.scale}, separate actual Git executable count`;
  } else {
    throw new Error("expected --scale 1|10 --cold|--warm, or --journey-time");
  }
  if (!report(value, `invocation ${record.build.run_id}: ${description}`)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`recoverable telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
