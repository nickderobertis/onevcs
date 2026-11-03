#!/usr/bin/env node
// Report one sanitise call's recorded wall time; no onevcs, git, fixture or timer here.
import { report } from "@onebudgetspec/sdk";
import { journeyCommand, readSanitizeRecord } from "./boundary-sanitize-budget-record.mjs";

if (process.argv.length > 2) {
  console.error(`boundary sanitize telemetry: expected no arguments, got ${process.argv.slice(2).join(" ")}\n` +
    "next: run it as 'node scripts/boundary-sanitize-budget.mjs' from crates/onevcs, as 'just budgets' does");
  process.exit(2);
}
try {
  const record = readSanitizeRecord();
  const seconds = us => (us / 1e6).toFixed(3);
  const { covered, phases } = record;
  // Detail carries no threshold of its own: the phases, the check beside it, the peak.
  const detail = `invocation ${record.build.run_id}: one boundary sanitize process over ${covered.identities} ` +
    `private repositories (${covered.terms} terms) and ${covered.levels} level files, a ${(covered.bytes / 1024).toFixed(0)} KiB ` +
    `body and ${covered.metadata} metadata entries, ${covered.replaced} spans rewritten; phases: derivation and level reads ` +
    `${seconds(phases.derivation_us)}s, matcher build ${seconds(phases.matcher_build_us)}s ` +
    `(${covered.matcher_builds} build), replacement ${seconds(phases.replacement_us)}s (${covered.replacement_passes} pass), ` +
    `re-match ${seconds(phases.matching_us)}s (${covered.rematches}); the same input through boundary check ` +
    `${seconds(record.check_wall_us)}s; peak RSS ${(record.sanitize_peak_rss_kib / 1024).toFixed(1)} MiB; at load1 ${record.load1}`;
  if (!report(record.sanitize_wall_us / 1e6, detail)) {
    throw new Error("ONEBUDGETSPEC_RESULT is missing; invoke through 'just budgets'");
  }
} catch (error) {
  console.error(`boundary sanitize telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
