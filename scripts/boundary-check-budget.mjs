#!/usr/bin/env node
// Report the boundary check's recorded latency; no onevcs, git, fixture or timer here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readBoundaryRecord } from "./boundary-check-budget-record.mjs";

try {
  if (process.argv.length > 2) throw new Error("expected no arguments");
  const record = readBoundaryRecord();
  const seconds = us => (us / 1e6).toFixed(3);
  const mib = bytes => (bytes / 1048576).toFixed(1);
  const { covered, check } = record;
  // Detail carries no threshold of its own: the phases, the peak, and one export.
  const detail = `invocation ${record.build.run_id}: one publication term check over ${covered.identities} ` +
    `private repositories (${covered.terms} terms), ${covered.paths} paths, ${mib(covered.bytes)} MiB; ` +
    `phases: derivation ${seconds(check.derivation_us)}s, diff ${seconds(check.diff_us)}s, ` +
    `matcher build ${seconds(check.matcher_build_us)}s, matching ${seconds(check.matching_us)}s; ` +
    `check process peak RSS ${mib(record.check_peak_rss_kib * 1024)} MiB; ` +
    `one export of the same files ${(record.export_wall_ms / 1000).toFixed(3)}s; at load1 ${record.load1}`;
  if (!report(check.total_us / 1e6, detail)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`boundary telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
