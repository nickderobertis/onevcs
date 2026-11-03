// Read-only validation of the sanitise call's record, shared by its budget entrypoint and
// its reader journeys. Same producer, invocation and provenance as recovery's.
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
    const schema = JSON.parse(readFileSync(new URL("./boundary-sanitize-budget.schema.json", import.meta.url)));
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

export function readSanitizeRecord(directory = recordDirectory, sourceRoot = repositoryRoot) {
  const { schema, validate } = schemaValidator();
  const read = name => JSON.parse(readFileSync(`${directory}/${name}`, "utf8"));
  const expected = read("recoverable-invocation.json");
  if (expected.state !== "complete" || !digest(expected.run_id)) {
    throw new Error("producing invocation is missing or incomplete");
  }
  const record = read("boundary-sanitize.json");
  if (!validate(record)) throw new Error(`incomplete telemetry: ${JSON.stringify(validate.errors)}`);
  if (record.version !== 1 || record.scenario !== "sanitize-call") throw new Error("foreign telemetry format or scenario");
  if (record.build.run_id !== expected.run_id || record.build.binary !== expected.binary ||
      record.build.binary_sha256 !== expected.binary_sha256 || record.build.source_sha256 !== expected.source_sha256) throw new Error("stale or foreign invocation/build");
  if (!digest(record.build.source_sha256) || record.build.source_sha256 !== sourceFingerprint(sourceRoot)) {
    throw new Error("stale source/build inputs");
  }
  if (record.build.binary !== "onevcs" || !digest(record.build.binary_sha256) || sha(readFileSync(`${directory}/${record.build.binary}`)) !== record.build.binary_sha256) {
    throw new Error("current binary does not match the producing build");
  }
  // The workload the budget names, or the figure is about some other write.
  const workload = schema["x-workload"];
  const covered = record.covered;
  if (covered.identities !== workload.identities || covered.terms < workload.terms || covered.levels !== workload.levels ||
      covered.bytes < workload.bytes || covered.metadata !== workload.metadata || covered.replaced !== workload.hits) {
    throw new Error("foreign or shrunk workload");
  }
  // The efficiency ruling: one matcher build, one replacement pass, one re-match.
  if (covered.matcher_builds !== 1 || covered.replacement_passes !== 1 || covered.rematches !== 1) {
    throw new Error("the call made more than one matcher pass");
  }
  const phases = record.phases;
  const parts = ["derivation_us", "matcher_build_us", "replacement_us", "matching_us"];
  if (phases.total_us <= 0 || parts.reduce((sum, part) => sum + phases[part], 0) > phases.total_us ||
      record.sanitize_wall_us < phases.total_us) {
    throw new Error("incomplete sanitise timing");
  }
  if (record.sanitize_peak_rss_kib <= 0 || record.check_wall_us <= 0 || record.preparation_ms <= 0 ||
      !Number.isFinite(record.load1) || record.load1 < 0) {
    throw new Error("incomplete detail");
  }
  return record;
}
