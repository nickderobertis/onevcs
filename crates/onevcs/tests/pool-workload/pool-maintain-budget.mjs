// Analyze the completed workload's recording; the journey owns measurement.
import { readFileSync } from "node:fs";
import { report } from "@onebudgetspec/sdk";

// llmlint: ignore-block[boundary_inputs_validated] This task requires direct telemetry analysis; the journey writes the record and onebudgetspec owns schema, numeric-result and threshold validation, so a second validator here is forbidden.
const record = JSON.parse(readFileSync(new URL("../../../../target/budget-records/pool-maintain.json", import.meta.url), "utf8"));
report(record.wall_seconds - record.command_seconds,
  `resolving ${record.resolving_seconds}s; claiming ${record.claiming_seconds}s; ` +
  `measuring before ${record.before_seconds}s (${record.before_entries} entries); ` +
  `measuring after ${record.after_seconds}s (${record.after_entries} entries); ` +
  `recording ${record.recording_seconds}s`);
// llmlint: ignore-end[boundary_inputs_validated]
