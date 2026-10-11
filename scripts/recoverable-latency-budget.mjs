#!/usr/bin/env node
// Report validated in-process latency; no subprocess, fixture preparation or timing here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readRecord } from "./recoverable-budget-record.mjs";

// A command this reader cannot answer is the caller's to fix, not the telemetry's.
class Usage extends Error {}
// A reader run outside onebudgetspec has nowhere to report to.
class Unwired extends Error {}

try {
  const args = process.argv.slice(2);
  if (!(args.length === 3 && args[0] === "--scale" && ["1", "10"].includes(args[1]) && ["--cold", "--warm"].includes(args[2]))) {
    throw new Usage(`expected --scale 1|10 --cold|--warm, got ${JSON.stringify(args)}`);
  }
  const mode = args[2].slice(2);
  if (mode === "cold" && args[1] !== "1") {
    throw new Usage("in-process cold calls are recorded at --scale 1 only");
  }
  const record = readRecord();
  const workload = record.workloads.find(row => row.scale === Number(args[1]));
  const calls = workload[`in_process_${mode}`];
  if (!calls) throw new Error(`no in-process ${mode} calls were recorded at scale ${workload.scale}`);
  const slowest = Math.max(...calls.map(call => call.wall_ms));
  const loads = calls.map(call => call.load1);
  const description = `${mode} in-process launcher Decision, scale ${workload.scale}: slowest of ${calls.length} calls ${slowest.toFixed(1)}ms, load1 ${Math.min(...loads)}-${Math.max(...loads)}`;
  if (!report(slowest / 1000, `invocation ${record.build.run_id}: ${description}`)) {
    throw new Unwired("ONEBUDGETSPEC_RESULT is missing, so there is nowhere to report to");
  }
} catch (error) {
  const usage = error instanceof Usage || error instanceof Unwired;
  const next = error instanceof Usage
    ? "pass --scale 1 --warm, --scale 10 --warm or --scale 1 --cold, as budgets.yaml's latency entries do"
    : error instanceof Unwired
      ? "run 'just budgets', which runs this reader with the result path to report to"
      : `run '${journeyCommand}' to regenerate complete current-build records`;
  console.error(`recoverable telemetry: ${error.message}\nnext: ${next}`);
  process.exitCode = usage ? 2 : 1;
}
