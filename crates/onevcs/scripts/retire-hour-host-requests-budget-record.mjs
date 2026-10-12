// Read-only validation of the retirement hour's record, shared by its budget entrypoint
// and its reader journeys. Same producer, invocation and provenance as recovery's.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import Ajv from "ajv/dist/2020.js";
import { repositoryRoot, sourceFingerprint } from "../../../scripts/recoverable-build.mjs";

// Compiled on first read rather than at import, so a missing or broken schema reaches
// the caller's error handler and its next action instead of failing module loading.
let compiled;
function schemaValidator() {
  if (!compiled) {
    const schema = JSON.parse(readFileSync(new URL("./retire-hour-host-requests-budget.schema.json", import.meta.url)));
    const ajv = new Ajv({ strict: false });
    for (const format of ["uint", "uint32", "uint64"]) ajv.addFormat(format, { type: "number", validate: n => Number.isSafeInteger(n) && n >= 0 });
    ajv.addFormat("double", { type: "number", validate: Number.isFinite });
    compiled = { schema, validate: ajv.compile(schema) };
  }
  return compiled;
}
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const digest = text => typeof text === "string" && /^[a-f0-9]{64}$/.test(text);
export const journeyCommand = "just recoverable-journeys";
export const recordDirectory = fileURLToPath(new URL("../../../target/budget-records/", import.meta.url));

export function readRetireHourRecord(directory = recordDirectory, sourceRoot = repositoryRoot) {
  const { schema, validate } = schemaValidator();
  const read = name => JSON.parse(readFileSync(`${directory}/${name}`, "utf8"));
  const expected = read("recoverable-invocation.json");
  if (expected.state !== "complete" || !digest(expected.run_id)) {
    throw new Error("producing invocation is missing or incomplete");
  }
  const record = read("retire-hour.json");
  if (!validate(record)) throw new Error(`incomplete telemetry: ${JSON.stringify(validate.errors)}`);
  if (record.version !== 1) throw new Error("foreign telemetry format");
  if (record.build.run_id !== expected.run_id || record.build.binary !== expected.binary ||
      record.build.binary_sha256 !== expected.binary_sha256 || record.build.source_sha256 !== expected.source_sha256) throw new Error("stale or foreign invocation/build");
  if (!digest(record.build.source_sha256) || record.build.source_sha256 !== sourceFingerprint(sourceRoot)) {
    throw new Error("stale source/build inputs");
  }
  if (record.build.binary !== "onevcs" || !digest(record.build.binary_sha256) || sha(readFileSync(`${directory}/${record.build.binary}`)) !== record.build.binary_sha256) {
    throw new Error("current binary does not match the producing build");
  }
  // The hour the budget names, or the figure is about some other hour.
  const workload = schema["x-workload"];
  if (record.identities !== workload.identities) throw new Error(`foreign workload: ${record.identities} identities, not ${workload.identities}`);
  const kinds = record.runs.map(run => run.kind).join();
  const expectedKinds = [...Array(workload.passes).fill("pass"), ...Array(workload.sweeps).fill("sweep")].join();
  if (kinds !== expectedKinds) throw new Error(`foreign or missing runs: ${kinds}`);
  for (const run of record.runs) {
    if (run.requests !== run.graphql_points + run.rest_requests ||
        run.requests !== run.merged_views + run.open_lists + run.merge_times + run.other ||
        run.examined <= 0 || run.wall_ms <= 0) throw new Error(`incomplete ${run.kind} telemetry`);
  }
  if (record.requests !== record.runs.reduce((sum, run) => sum + run.requests, 0)) throw new Error("the hour's total is not its runs' sum");
  if (!Number.isFinite(record.load1) || record.load1 < 0) throw new Error("incomplete detail");
  return record;
}
