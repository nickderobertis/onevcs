// Read-only validation of the draft hold's record, shared by its budget entrypoint and its
// reader journeys. Same producer, invocation and provenance as recovery's.
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
    const schema = JSON.parse(readFileSync(new URL("./draft-early-lift-budget.schema.json", import.meta.url)));
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

export function readDraftHoldRecord(directory = recordDirectory, sourceRoot = repositoryRoot) {
  const { schema, validate } = schemaValidator();
  const read = name => JSON.parse(readFileSync(`${directory}/${name}`, "utf8"));
  const expected = read("recoverable-invocation.json");
  if (expected.state !== "complete" || !digest(expected.run_id)) {
    throw new Error("producing invocation is missing or incomplete");
  }
  const record = read("draft-hold.json");
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
  // The workload the budget names, or the figure is about some other watch.
  const workload = schema["x-workload"];
  for (const key of ["scale", "pace", "poll_seconds", "grace_seconds", "wait_seconds", "reuse_seconds"]) {
    if (record[key] !== workload[key]) throw new Error(`foreign workload: ${key} is ${record[key]}, not ${workload[key]}`);
  }
  const shapes = record.scenarios.map(scenario => scenario.shape);
  if (shapes.join() !== workload.shapes.join()) throw new Error(`foreign or missing scenarios: ${shapes.join(", ")}`);
  // The two watches that run their whole wait make the real watch's readings.
  for (const scenario of record.scenarios.filter(scenario => ["two-phase", "never-idle"].includes(scenario.shape))) {
    if (scenario.polls !== workload.polls) throw new Error(`shrunk workload: ${scenario.shape} made ${scenario.polls} polls, not ${workload.polls}`);
  }
  for (const scenario of record.scenarios) {
    if (scenario.polls <= 0 || scenario.check_read_requests < scenario.polls || scenario.watch_requests < scenario.check_read_requests ||
        scenario.activity_reads <= 0 || scenario.activity_requests < scenario.activity_reads || scenario.wall_ms <= 0) {
      throw new Error(`incomplete ${scenario.shape} telemetry`);
    }
  }
  if (!Number.isFinite(record.load1) || record.load1 < 0) throw new Error("incomplete detail");
  return record;
}
