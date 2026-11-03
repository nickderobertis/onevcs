import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync, copyFileSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readExportRecord, repeatedBytes } from "./export-budget-record.mjs";
import { sourceFingerprint, inputFiles, repositoryRoot } from "../../../scripts/recoverable-build.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
function fixture(kind) {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(), "export-reader-"));
  writeFileSync(join(root, "onevcs"), "CLI");
  writeFileSync(join(root, "export-workload"), "optimized producer");
  const schema = JSON.parse(readFileSync(new URL(`./export-${kind}-budget.schema.json`, import.meta.url)));
  const build = { run_id: sha("invocation"), binary: "onevcs", binary_sha256: sha("CLI"), source_sha256: sourceFingerprint() };
  const covered = schema["x-workload"];
  const record = { version: 2, build, producer_sha256: sha("optimized producer"), covered, load1: 1,
    scenario: kind === "wall-time" ? "realistic-export" : "realistic-export-source-lfs" };
  if (kind === "wall-time") {
    record.wall_us = 1400000;
    record.peak_rss_kib = 300000;
    record.phases = Object.fromEntries(Object.keys(schema.properties.phases.properties).map(key => [key, 1]));
    Object.assign(record.phases, {total_us: 1300000, paths: covered.paths, bytes: covered.bytes, lfs_objects: covered.lfs_objects, lfs_bytes: covered.lfs_bytes});
  } else {
    const completed = Object.fromEntries(Array.from({length: covered.lfs_objects}, (_, n) => [sha(`object-${n}`), Math.floor(covered.lfs_bytes / covered.lfs_objects) + Number(n < covered.lfs_bytes % covered.lfs_objects)]));
    record.object_sizes = {...completed};
    record.attempts = [{objects:covered.lfs_objects, bytes:covered.lfs_bytes, progress_lines:covered.lfs_objects, completed}, {objects:0, bytes:0, progress_lines:0, completed:{}}];
  }
  const invocation = { state: "complete", ...build };
  const name = kind === "wall-time" ? "export.json" : "export-source-lfs.json";
  const save = () => {
    writeFileSync(join(root, name), JSON.stringify(record));
    writeFileSync(join(root, "recoverable-invocation.json"), JSON.stringify(invocation));
  };
  save();
  return {root, record, invocation, save, name};
}
for (const kind of ["wall-time", "source-lfs-download"]) {
  test(`${kind}: accepts completed provenance`, () => {
    const f = fixture(kind);
    try { assert.deepEqual(readExportRecord(kind, f.root), f.record); }
    finally { rmSync(f.root, {recursive:true, force:true}); }
  });
  const refusals = {
    "incomplete producer": f => { f.invocation.state = "failed"; f.save(); },
    "foreign invocation": f => { f.invocation.run_id = sha("foreign"); f.save(); },
    "stale source": f => { f.record.build.source_sha256 = sha("old"); f.invocation.source_sha256 = sha("old"); f.save(); },
    "stale producer": f => writeFileSync(join(f.root, "export-workload"), "new producer"),
    "stale CLI": f => writeFileSync(join(f.root, "onevcs"), "new CLI"),
    "missing record": f => rmSync(join(f.root, f.name)),
    "incomplete record": f => { delete f.record.covered; f.save(); },
    "shrunk workload": f => { f.record.covered.paths--; f.save(); },
    "foreign scenario": f => { f.record.scenario = "foreign"; f.save(); },
    "unknown field": f => { f.record.extra = true; f.save(); },
  };
  for (const [name, mutate] of Object.entries(refusals)) test(`${kind}: refuses ${name}`, () => {
    const f = fixture(kind);
    try { mutate(f); assert.throws(() => readExportRecord(kind, f.root)); }
    finally { rmSync(f.root, {recursive:true, force:true}); }
  });
  test(`${kind}: real reader entrypoint writes the SDK measurement and refuses missing telemetry`, () => {
    const f = fixture(kind);
    try {
      mkdirSync(join(f.root, "target/budget-records"), {recursive:true});
      symlinkSync(fileURLToPath(new URL("../../../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
      for (const path of inputFiles()) {
        mkdirSync(join(f.root, path, ".."), {recursive:true});
        copyFileSync(join(repositoryRoot, path), join(f.root, path));
      }
      for (const name of ["onevcs", "export-workload", f.name, "recoverable-invocation.json"]) copyFileSync(join(f.root, name), join(f.root, "target/budget-records", name));
      const script = join(f.root, `crates/onevcs/scripts/export-${kind}-budget.mjs`);
      const result = join(f.root, "result.json");
      const env = {PATH:"", ONEBUDGETSPEC_RESULT:result};
      const accepted = spawnSync(process.execPath, [script], {encoding:"utf8", env});
      assert.equal(accepted.status, 0, accepted.stderr);
      const output = JSON.parse(readFileSync(result));
      assert.equal(output.value, kind === "wall-time" ? 1.4 : 0);
      assert.match(output.detail, /load1 1/);
      rmSync(join(f.root, "target/budget-records", f.name));
      const refused = spawnSync(process.execPath, [script], {encoding:"utf8", env});
      assert.equal(refused.status, 1);
      assert.match(refused.stderr, /just recoverable-journeys/);
    } finally { rmSync(f.root, {recursive:true, force:true}); }
  });
}
test("source reader preserves overruns for the budget checker to judge", () => {
  const f = fixture("source-lfs-download");
  try {
    const oid = Object.keys(f.record.attempts[0].completed)[0];
    f.record.attempts[1] = {objects:1, bytes:100, progress_lines:1, completed:{[oid]:100}};
    f.save();
    assert.equal(repeatedBytes(readExportRecord("source-lfs-download", f.root)), 100);
  } finally { rmSync(f.root, {recursive:true, force:true}); }
});

test("source reader refuses incomplete object sizes and transfer totals", () => {
  for (const mutate of [
    record => { delete record.object_sizes[Object.keys(record.object_sizes)[0]]; },
    record => { record.attempts[0].bytes--; },
    record => { record.object_sizes[Object.keys(record.object_sizes)[0]]--; },
  ]) {
    const f = fixture("source-lfs-download");
    try { mutate(f.record); f.save(); assert.throws(() => readExportRecord("source-lfs-download", f.root)); }
    finally { rmSync(f.root, {recursive:true, force:true}); }
  }
});

test("repeated downloads within the cold attempt count beyond one copy", () => {
  const f = fixture("source-lfs-download");
  try {
    const oid = Object.keys(f.record.object_sizes)[0];
    const size = f.record.object_sizes[oid];
    f.record.attempts[0].completed[oid] += size;
    f.record.attempts[0].bytes += size;
    f.record.attempts[0].progress_lines++;
    f.save();
    assert.equal(repeatedBytes(readExportRecord("source-lfs-download", f.root)), size);
  } finally { rmSync(f.root, {recursive:true, force:true}); }
});
