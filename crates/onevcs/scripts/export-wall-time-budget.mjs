#!/usr/bin/env node
// Report the optimized in-process call; this reader never runs an export.
import { report } from "@onebudgetspec/sdk";
import { readExportRecord, journeyCommand } from "./export-budget-record.mjs";
try {
  if (process.argv.length !== 2) throw new Error("expected no arguments");
  const record = readExportRecord("wall-time");
  const { covered, phases } = record;
  const phaseNames = ["diff_us", "derivation_us", "matcher_build_us", "extraction_us", "matching_us", "lfs_us", "tree_write_us"];
  const detail = `invocation ${record.build.run_id}: ${covered.identities} private repositories (${covered.terms} terms); ${covered.paths} paths, ${covered.bytes} raw bytes, ` +
    `${phases.decoded_bytes} decoded bytes, ${phases.opaque_files} opaque files; ${phases.workers} workers; ` +
    `${phases.lfs_objects} LFS objects, ${phases.lfs_bytes} LFS bytes; ` +
    phaseNames.map(name => `${name}=${phases[name]}`).join(", ") +
    `; peak RSS ${record.peak_rss_kib} KiB; load1 ${record.load1}`;
  if (!report(record.wall_us / 1e6, detail)) throw new Error("ONEBUDGETSPEC_RESULT is missing; run through just budgets");
} catch (error) {
  console.error(`export telemetry: ${error.message}\nnext: run '${journeyCommand}' to regenerate complete current-build records`);
  process.exitCode = 1;
}
