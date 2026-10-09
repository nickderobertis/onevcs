#!/usr/bin/env node
// Report validated in-process latency; no subprocess, fixture preparation or timing here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readRecord } from "./recoverable-budget-record.mjs";

try {
  const args = process.argv.slice(2);
  if (!(args.length === 3 && args[0] === "--scale" && ["1", "10"].includes(args[1]) && ["--cold", "--warm"].includes(args[2]))) {
    throw new Error("expected --scale 1|10 --cold|--warm");
  }
  const record = readRecord();
  const workload = record.workloads.find(row => row.scale === Number(args[1]));
  const mode = args[2].slice(2);
  const calls = workload[`in_process_${mode}`];
  if (!calls) throw new Error(`no in-process ${mode} calls were recorded at scale ${workload.scale}`);
  const slowest = Math.max(...calls.map(call => call.wall_ms));
  const loads = calls.map(call => call.load1);
  const description = `${mode} in-process launcher Decision, scale ${workload.scale}: slowest of ${calls.length} calls ${slowest.toFixed(1)}ms, load1 ${Math.min(...loads)}-${Math.max(...loads)}`;
  if (!report(slowest / 1000, `invocation ${record.build.run_id}: ${description}`)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`recoverable telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
