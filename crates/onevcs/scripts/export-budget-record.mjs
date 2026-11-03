// Read-only export telemetry, bound to recovery's invocation and both producing binaries.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import Ajv from "ajv/dist/2020.js";
import { repositoryRoot, sourceFingerprint } from "../../../scripts/recoverable-build.mjs";

export const recordDirectory = fileURLToPath(new URL("../../../target/budget-records/", import.meta.url));
export const journeyCommand = "just export-journeys";
const digest = text => typeof text === "string" && /^[a-f0-9]{64}$/.test(text);
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const validators = new Map();
function validator(kind) {
  if (!validators.has(kind)) {
    const schema = JSON.parse(readFileSync(new URL(`./export-${kind}-budget.schema.json`, import.meta.url)));
    const ajv = new Ajv({ strict: true });
    ajv.addKeyword("x-workload");
    validators.set(kind, { schema, validate: ajv.compile(schema) });
  }
  return validators.get(kind);
}

export function readExportRecord(kind, directory = recordDirectory, sourceRoot = repositoryRoot) {
  if (!["wall-time", "source-lfs-download"].includes(kind)) throw new Error("unknown export record kind");
  const { schema, validate } = validator(kind);
  const read = name => JSON.parse(readFileSync(`${directory}/${name}`, "utf8"));
  const invocation = read("export-invocation.json");
  if (invocation.state !== "complete" || !digest(invocation.run_id)) throw new Error("producing invocation is incomplete");
  const record = read(kind === "wall-time" ? "export.json" : "export-source-lfs.json");
  if (!validate(record)) throw new Error(`incomplete export telemetry: ${JSON.stringify(validate.errors)}`);
  const build = record.build;
  for (const key of ["run_id", "binary", "binary_sha256", "source_sha256"]) {
    if (build[key] !== invocation[key]) throw new Error("stale or foreign invocation/build");
  }
  if (!digest(build.source_sha256) || build.source_sha256 !== sourceFingerprint(sourceRoot, "scripts/export-build-inputs.json")) throw new Error("stale source/build inputs");
  if (build.binary !== "export-onevcs" || !digest(build.binary_sha256) || sha(readFileSync(`${directory}/export-onevcs`)) !== build.binary_sha256) throw new Error("foreign CLI binary");
  if (!digest(record.producer_sha256) || sha(readFileSync(`${directory}/export-workload`)) !== record.producer_sha256) throw new Error("foreign producing binary");
  for (const [key, expected] of Object.entries(schema["x-workload"])) {
    if (record.covered[key] !== expected) throw new Error(`foreign or shrunk workload: ${key}`);
  }
  if (kind === "wall-time") {
    const phases = record.phases;
    const times = ["diff_us", "derivation_us", "matcher_build_us", "extraction_us", "matching_us", "lfs_us", "tree_write_us"];
    if (times.some(key => phases[key] > phases.total_us) || record.wall_us < phases.total_us || phases.workers < 1 || phases.paths !== record.covered.paths || phases.bytes !== record.covered.bytes || phases.lfs_objects !== record.covered.lfs_objects || phases.lfs_bytes !== record.covered.lfs_bytes || phases.identities !== record.covered.identities || phases.terms !== record.covered.terms) throw new Error("incomplete export diagnostics");
  } else {
    const [cold, retry] = record.attempts;
    if (cold.objects !== record.covered.lfs_objects) throw new Error("incomplete cold source downloads");
    for (const attempt of [cold, retry]) {
      if (attempt.identities !== record.covered.identities || attempt.terms !== record.covered.terms) throw new Error("foreign actual source scope");
      const objects = Object.entries(attempt.completed);
      if ((attempt.counted_from === "git-lfs-progress" && attempt.progress_lines < attempt.completion_events) ||
          (attempt.counted_from === "git-lfs-trace-complete" && (attempt.progress_lines !== 0 || attempt.trace_lines < attempt.completion_events))) throw new Error("incomplete client transfer evidence");
      if (objects.some(([oid, bytes]) => !record.object_sizes[oid] || bytes % record.object_sizes[oid] !== 0) ||
          objects.reduce((sum, [oid, bytes]) => sum + bytes / record.object_sizes[oid], 0) !== attempt.completion_events) throw new Error("incomplete completed-transfer accounting");
      if (objects.some(([oid]) => !digest(oid)) || objects.length !== attempt.objects || objects.reduce((sum, [, bytes]) => sum + bytes, 0) !== attempt.bytes) throw new Error("incomplete download accounting");
    }
    if (Object.values(record.object_sizes).reduce((sum, bytes) => sum + bytes, 0) !== record.covered.lfs_bytes) throw new Error("shrunk source object bytes");
    if (Object.keys(record.object_sizes).length !== record.covered.lfs_objects || Object.entries(record.object_sizes).some(([oid, size]) => !digest(oid) || !(oid in cold.completed) || cold.completed[oid] < size)) throw new Error("incomplete source object inventory");
    if (Object.keys(retry.completed).some(oid => !(oid in cold.completed))) throw new Error("retry downloaded a foreign object");
  }
  return record;
}

export function repeatedBytes(record) {
  return Object.entries(record.object_sizes).reduce((sum, [oid, size]) =>
    sum + Math.max(0, record.attempts.reduce((bytes, attempt) => bytes + (attempt.completed[oid] || 0), 0) - size), 0);
}
