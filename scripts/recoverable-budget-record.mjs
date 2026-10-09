// Read-only validation shared by the budget entrypoint and its filesystem journeys.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import Ajv from "ajv/dist/2020.js";
import { repositoryRoot, sourceFingerprint } from "./recoverable-build.mjs";

// Compiled on first read rather than at import, so a missing or broken schema reaches
// the caller's error handler and its next action instead of failing module loading.
let compiled;
function schemaValidator() {
  if (!compiled) {
    const schema = JSON.parse(readFileSync(new URL("./recoverable-budget.schema.json", import.meta.url)));
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
export const recordDirectory = fileURLToPath(new URL("../target/budget-records/", import.meta.url));

export function readRecord(directory = recordDirectory, sourceRoot = repositoryRoot) {
  const { schema, validate } = schemaValidator();
  const read = name => JSON.parse(readFileSync(`${directory}/${name}`, "utf8"));
  const expected = read("recoverable-invocation.json");
  if (expected.state !== "complete" || !digest(expected.run_id)) {
    throw new Error("producing invocation is missing or incomplete");
  }
  const record = read("recoverable.json");
  if (!validate(record)) throw new Error(`incomplete telemetry: ${JSON.stringify(validate.errors)}`);
  if (record.version !== 1 || record.scenario !== "launcher-decision") throw new Error("foreign telemetry format or scenario");
  if (record.build.run_id !== expected.run_id || record.build.binary !== expected.binary ||
      record.build.binary_sha256 !== expected.binary_sha256 || record.build.source_sha256 !== expected.source_sha256) throw new Error("stale or foreign invocation/build");
  if (!digest(record.build.source_sha256) || record.build.source_sha256 !== sourceFingerprint(sourceRoot)) {
    throw new Error("stale source/build inputs");
  }
  if (record.build.binary !== "onevcs" || !digest(record.build.binary_sha256) || sha(readFileSync(`${directory}/${record.build.binary}`)) !== record.build.binary_sha256) {
    throw new Error("current binary does not match the producing build");
  }
  if (record.preparation_ms <= 0 || record.total_ms < record.preparation_ms || !Number.isFinite(record.load1) || record.load1 < 0) {
    throw new Error("incomplete preparation/total timing");
  }
  for (const [index, workload] of record.workloads.entries()) {
    const expectedScale = index === 0 ? 1 : 10;
    if (workload.scale !== expectedScale || workload.shape.some(n => n <= 0) ||
        Object.keys(workload.class_counts).sort().join(",") !== "in-part,landed,live,no,retirable,superseded,unknown" ||
        Object.values(workload.class_counts).some(n => !Number.isSafeInteger(n) || n <= 0)) {
      throw new Error("foreign scale/workload or missing recovery class");
    }
    const sizes = schema["x-workloads"][String(expectedScale)];
    if (workload.shape.some((n, i) => n !== sizes[i])) throw new Error("foreign workload");
    for (const mode of ["cold", "warm"]) {
      const sample = workload[mode];
      if (sample.rows <= 0 || sample.wall_ms <= 0 || !Number.isFinite(sample.load1) || sample.load1 < 0 || !digest(sample.verdict_sha256)) {
        throw new Error(`incomplete ${mode} sample`);
      }
      if (sample.verdict_sha256 !== workload.counted_verdict_sha256) throw new Error("timed/counted verdicts disagree");
    }
    const decisionRows = ["no", "unknown", "in-part", "live", "superseded"].reduce((sum, name) => sum + workload.class_counts[name], 0);
    if (workload.cold.rows !== decisionRows || workload.cold.rows !== workload.warm.rows || workload.cold_git <= 0 || workload.warm_git <= 0) {
      throw new Error("incomplete counting call");
    }
  }
  return record;
}
