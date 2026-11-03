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
  writeFileSync(join(root, "export-onevcs"), "CLI");
  writeFileSync(join(root, "export-workload"), "optimized producer");
  const schema = JSON.parse(readFileSync(new URL(`./export-${kind}-budget.schema.json`, import.meta.url)));
  const build = { run_id: sha("invocation"), binary: "export-onevcs", binary_sha256: sha("CLI"), source_sha256: sourceFingerprint(undefined, "scripts/export-build-inputs.json") };
  const covered = schema["x-workload"];
  const record = { version: 4, build, producer_sha256: sha("optimized producer"), covered, load1: 1,
    scenario: kind === "wall-time" ? "realistic-export" : "realistic-export-source-lfs" };
  if (kind === "wall-time") {
    record.wall_us = 1400000;
    record.peak_rss_kib = 300000;
    record.phases = Object.fromEntries(Object.keys(schema.properties.phases.properties).map(key => [key, 1]));
    Object.assign(record.phases, {total_us: 1300000, paths: covered.paths, bytes: covered.bytes, lfs_objects: covered.lfs_objects, lfs_bytes: covered.lfs_bytes, identities: covered.identities, terms: covered.terms});
  } else {
    const completed = Object.fromEntries(Array.from({length: covered.lfs_objects}, (_, n) => [sha(`object-${n}`), Math.floor(covered.lfs_bytes / covered.lfs_objects) + Number(n < covered.lfs_bytes % covered.lfs_objects)]));
    record.object_sizes = {...completed};
    record.attempts = [{objects:covered.lfs_objects, bytes:covered.lfs_bytes, progress_lines:0, trace_lines:covered.lfs_objects * 2, counted_from:"git-lfs-trace-complete", identities:covered.identities, terms:covered.terms, completion_events:covered.lfs_objects, completed}, {objects:0, bytes:0, progress_lines:0, trace_lines:1, counted_from:"git-lfs-trace-complete", identities:covered.identities, terms:covered.terms, completion_events:0, completed:{}}];
  }
  const invocation = { state: "complete", ...build };
  const name = kind === "wall-time" ? "export.json" : "export-source-lfs.json";
  const save = () => {
    writeFileSync(join(root, name), JSON.stringify(record));
    writeFileSync(join(root, "export-invocation.json"), JSON.stringify(invocation));
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
    "stale CLI": f => writeFileSync(join(f.root, "export-onevcs"), "new CLI"),
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
      for (const path of inputFiles(undefined, "scripts/export-build-inputs.json")) {
        mkdirSync(join(f.root, path, ".."), {recursive:true});
        copyFileSync(join(repositoryRoot, path), join(f.root, path));
      }
      for (const name of ["export-onevcs", "export-workload", f.name, "export-invocation.json"]) copyFileSync(join(f.root, name), join(f.root, "target/budget-records", name));
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
      assert.match(refused.stderr, /just export-journeys/);
    } finally { rmSync(f.root, {recursive:true, force:true}); }
  });
}
test("source reader preserves overruns for the budget checker to judge", () => {
  const f = fixture("source-lfs-download");
  try {
    const oid = Object.keys(f.record.attempts[0].completed)[0];
    const size = f.record.object_sizes[oid];
    f.record.attempts[1] = {objects:1, bytes:size, progress_lines:0, trace_lines:2, counted_from:"git-lfs-trace-complete", identities:f.record.covered.identities, terms:f.record.covered.terms, completion_events:1, completed:{[oid]:size}};
    f.save();
    assert.equal(repeatedBytes(readExportRecord("source-lfs-download", f.root)), size);
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
    f.record.attempts[0].completion_events++;
    f.save();
    assert.equal(repeatedBytes(readExportRecord("source-lfs-download", f.root)), size);
  } finally { rmSync(f.root, {recursive:true, force:true}); }
});

test("source reader requires completed-transfer evidence for its selected counter", () => {
  for (const mutate of [
    record => { record.attempts[0].completion_events++; },
    record => { record.attempts[0].trace_lines = 0; },
    record => { record.attempts[0].counted_from = "queued-requests"; },
    record => { record.attempts[0].progress_lines = 1; },
    record => { record.attempts[0].counted_from = "git-lfs-progress"; },
  ]) {
    const f = fixture("source-lfs-download");
    try { mutate(f.record); f.save(); assert.throws(() => readExportRecord("source-lfs-download", f.root)); }
    finally { rmSync(f.root, {recursive:true, force:true}); }
  }
});

test("source reader also accepts actual byte-progress accounting", () => {
  const f = fixture("source-lfs-download");
  try {
    f.record.attempts[0].counted_from = "git-lfs-progress";
    f.record.attempts[0].progress_lines = f.record.attempts[0].completion_events;
    f.save();
    assert.equal(repeatedBytes(readExportRecord("source-lfs-download", f.root)), 0);
  } finally { rmSync(f.root, {recursive:true, force:true}); }
});

for (const kind of ["wall-time", "source-lfs-download"]) test(`${kind}: actual diagnostic scope must match the claimed workload`, () => {
  for (const key of ["identities", "terms"]) {
    for (const missing of [false, true]) {
      const f = fixture(kind);
      try {
        const actual = kind === "wall-time" ? f.record.phases : f.record.attempts[0];
        if (missing) delete actual[key]; else actual[key]--;
        f.save();
        assert.throws(() => readExportRecord(kind, f.root));
      } finally { rmSync(f.root, {recursive:true, force:true}); }
    }
  }
});

test("export and recovery fingerprints own disjoint journey and reader inputs", () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(), "export-inputs-"));
  try {
    const manifests = ["scripts/recoverable-build-inputs.json", "scripts/export-build-inputs.json"];
    const paths = new Set(manifests.flatMap(manifest => inputFiles(repositoryRoot, manifest)));
    for (const path of paths) {
      mkdirSync(join(root, path, ".."), {recursive:true});
      copyFileSync(join(repositoryRoot, path), join(root, path));
    }
    const hashes = () => manifests.map(manifest => sourceFingerprint(root, manifest));
    const before = hashes();
    const recovery = "crates/onevcs/tests/recovery-workload/main.rs";
    writeFileSync(join(root, recovery), readFileSync(join(root, recovery), "utf8") + "\n// recovery-only change\n");
    const afterRecovery = hashes();
    assert.notEqual(afterRecovery[0], before[0]);
    assert.equal(afterRecovery[1], before[1]);
    const exporter = "crates/onevcs/tests/recovery-workload/export/main.rs";
    writeFileSync(join(root, exporter), readFileSync(join(root, exporter), "utf8") + "\n// export-only change\n");
    const afterExport = hashes();
    assert.equal(afterExport[0], afterRecovery[0]);
    assert.notEqual(afterExport[1], afterRecovery[1]);
    const reader = "crates/onevcs/scripts/export-budget-record.mjs";
    writeFileSync(join(root, reader), readFileSync(join(root, reader), "utf8") + "\n// export reader change\n");
    const afterReader = hashes();
    assert.equal(afterReader[0], afterExport[0]);
    assert.notEqual(afterReader[1], afterExport[1]);
  } finally { rmSync(root, {recursive:true, force:true}); }
});

test("failed export invocation retracts only export evidence", () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(), "export-invocation-"));
  try {
    mkdirSync(join(root, "scripts"));
    mkdirSync(join(root, "target/budget-records"), {recursive:true});
    copyFileSync(new URL("../../../scripts/export-invocation.mjs", import.meta.url), join(root,"scripts/export-invocation.mjs"));
    for (const name of ["export.json", "export-source-lfs.json", "recoverable.json", "recoverable-invocation.json"]) writeFileSync(join(root,"target/budget-records",name), name);
    const result = spawnSync(process.execPath, [join(root,"scripts/export-invocation.mjs"), process.execPath, "-e", "process.exit(1)"], {encoding:"utf8"});
    assert.equal(result.status, 1);
    for (const name of ["export.json", "export-source-lfs.json"]) assert.throws(() => readFileSync(join(root,"target/budget-records",name)), /ENOENT/);
    for (const name of ["recoverable.json", "recoverable-invocation.json"]) assert.equal(readFileSync(join(root,"target/budget-records",name),"utf8"), name);
    assert.equal(JSON.parse(readFileSync(join(root,"target/budget-records/export-invocation.json"))).state,"failed");
  } finally { rmSync(root, {recursive:true, force:true}); }
});

test("export and recovery cache restores cannot overwrite one another", () => {
  const project = path => JSON.parse(readFileSync(new URL(path, import.meta.url)));
  const recovery = project("../tests/recovery-workload/project.json");
  const exporter = project("../tests/recovery-workload/export/project.json");
  for (const target of ["test", "test-quick"]) {
    for (const left of recovery.targets[target].outputs) for (const right of exporter.targets[target].outputs) {
      assert.ok(left !== right && !left.startsWith(`${right}/`) && !right.startsWith(`${left}/`), `${left} overlaps ${right}`);
    }
  }
});

for (const excludes of ["not-a-list", ["../escape"], ["/absolute"], ["a\\b"], [""]]) test(`manifest refuses malformed exclusions ${JSON.stringify(excludes)}`, () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(), "export-manifest-"));
  try {
    mkdirSync(join(root,"scripts"));
    writeFileSync(join(root,"scripts/export-build-inputs.json"), JSON.stringify({version:2,files:[],directories:[],prefixes:[],excludes}));
    assert.throws(() => inputFiles(root,"scripts/export-build-inputs.json"), /excludes/);
  } finally { rmSync(root,{recursive:true,force:true}); }
});
