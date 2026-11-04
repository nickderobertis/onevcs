#!/usr/bin/env node
// Report actual repeated source downloads, as git-lfs counted them.
import { report } from "@onebudgetspec/sdk";
import { readExportRecord, repeatedBytes, journeyCommand } from "./export-budget-record.mjs";
try {
  if (process.argv.length !== 2) throw new Error("expected no arguments");
  const record = readExportRecord("source-lfs-download");
  const detail = `invocation ${record.build.run_id}: ` + record.attempts.map((attempt, n) =>
    `attempt ${n + 1}: ${attempt.objects} objects, ${attempt.bytes} bytes, ${attempt.progress_lines} progress lines, ${attempt.trace_lines} trace lines, ${attempt.completion_events} completed transfers from ${attempt.counted_from}`).join("; ") + `; load1 ${record.load1}`;
  if (!report(repeatedBytes(record), detail)) throw new Error("ONEBUDGETSPEC_RESULT is missing; run through just budgets");
} catch (error) {
  console.error(`export source LFS telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
