import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync, copyFileSync, symlinkSync, existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readBoundaryRecord, recordDirectory } from "./boundary-check-budget-record.mjs";
import { sourceFingerprint, inputFiles, repositoryRoot } from "./recoverable-build.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const schema = JSON.parse(readFileSync(new URL("./boundary-check-budget.schema.json", import.meta.url)));
const scratch = () => process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir();
function fixture() {
  const root = mkdtempSync(join(scratch(), "boundary-reader-"));
  const binary = Buffer.from("the producing executable bytes");
  writeFileSync(join(root, "onevcs"), binary);
  const build = { run_id: sha("invocation"), binary: "onevcs", binary_sha256: sha(binary), source_sha256: sourceFingerprint() };
  const workload = schema["x-workload"];
  const record = {
    version: 1, build, scenario: "publication-term-check",
    covered: { identities: workload.identities, terms: workload.terms, commits: 2, paths: workload.paths, bytes: workload.bytes },
    check: { total_us: 200000, derivation_us: 20000, diff_us: 90000, matcher_build_us: 8000, matching_us: 30000 },
    check_peak_rss_kib: 30000, publication_wall_ms: 1500, export_wall_ms: 800, preparation_ms: 4000, load1: 1,
  };
  const invocation = { state: "complete", ...build };
  const save = () => {
    writeFileSync(join(root, "boundary.json"), JSON.stringify(record));
    writeFileSync(join(root, "recoverable-invocation.json"), JSON.stringify(invocation));
  };
  save();
  return { root, record, invocation, save };
}

test("reader accepts a completed matching record", () => {
  const f = fixture();
  try { assert.deepEqual(readBoundaryRecord(f.root), f.record); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

const refusals = {
  "a missing record": f => rmSync(join(f.root, "boundary.json")),
  "a malformed record": f => writeFileSync(join(f.root, "boundary.json"), "{broken"),
  "a missing invocation": f => rmSync(join(f.root, "recoverable-invocation.json")),
  "a failed producer": f => { f.invocation.state = "failed"; f.save(); },
  "an incomplete producer": f => { f.invocation.state = "started"; f.save(); },
  "a foreign invocation": f => { f.invocation.run_id = sha("another invocation"); f.save(); },
  "stale source inputs": f => { f.record.build.source_sha256 = sha("another source tree"); f.save(); },
  "a stale binary": f => writeFileSync(join(f.root, "onevcs"), "another build"),
  "a foreign scenario": f => { f.record.scenario = "other"; f.save(); },
  "a foreign format": f => { f.record.version = 2; f.save(); },
  "a shrunk scope": f => { f.record.covered.identities = 2; f.save(); },
  "fewer terms": f => { f.record.covered.terms = 200; f.save(); },
  "fewer paths": f => { f.record.covered.paths = 10; f.save(); },
  "fewer bytes": f => { f.record.covered.bytes = 1024; f.save(); },
  "no commits": f => { f.record.covered.commits = 0; f.save(); },
  "an untimed check": f => { f.record.check.total_us = 0; f.save(); },
  "phases longer than the check": f => { f.record.check.diff_us = 900000; f.save(); },
  "a missing phase": f => { delete f.record.check.matching_us; f.save(); },
  "a missing peak": f => { f.record.check_peak_rss_kib = 0; f.save(); },
  "a missing export": f => { f.record.export_wall_ms = 0; f.save(); },
  "an unknown field": f => { f.record.terms = ["anything"]; f.save(); },
};
for (const [name, mutate] of Object.entries(refusals)) test(`reader refuses ${name}`, () => {
  const f = fixture();
  try { mutate(f); assert.throws(() => readBoundaryRecord(f.root)); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the CLI reports the check's seconds and its detail through the SDK, and names the producer on refusal", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    symlinkSync(fileURLToPath(new URL("../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
    for (const path of inputFiles()) {
      mkdirSync(join(f.root, path, ".."), { recursive: true });
      copyFileSync(join(repositoryRoot, path), join(f.root, path));
    }
    for (const name of ["onevcs", "boundary.json", "recoverable-invocation.json"]) {
      copyFileSync(join(f.root, name), join(f.root, "target", "budget-records", name));
    }
    const result = join(f.root, "result.json");
    const run = () => spawnSync(process.execPath, [join(f.root, "scripts", "boundary-check-budget.mjs")], {
      encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: result },
    });
    const accepted = run();
    assert.equal(accepted.status, 0, accepted.stderr);
    const reported = JSON.parse(readFileSync(result, "utf8"));
    assert.equal(reported.value, 0.2);
    for (const phrase of ["derivation 0.020s", "diff 0.090s", "matcher build 0.008s", "matching 0.030s", "peak RSS", "export of the same files 0.800s"]) {
      assert.ok(reported.detail.includes(phrase), `${phrase}: ${reported.detail}`);
    }
    rmSync(join(f.root, "target", "budget-records", "boundary.json"));
    const refused = run();
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /boundary telemetry: /);
    assert.match(refused.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the invocation wrapper retracts the boundary record with recovery's when a producer starts or fails", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    copyFileSync(fileURLToPath(new URL("recoverable-invocation.mjs", import.meta.url)), join(f.root, "scripts", "recoverable-invocation.mjs"));
    copyFileSync(join(f.root, "boundary.json"), join(f.root, "target", "budget-records", "boundary.json"));
    const run = spawnSync(process.execPath, [join(f.root, "scripts", "recoverable-invocation.mjs"), process.execPath, "-e",
      "process.exit(require('node:fs').existsSync('target/budget-records/boundary.json') ? 2 : 1)"],
      { cwd: f.root, encoding: "utf8" });
    assert.equal(run.status, 1, "the record was retracted before the producer started: " + run.stderr);
    assert.ok(!existsSync(join(f.root, "target", "budget-records", "boundary.json")));
    assert.throws(() => readBoundaryRecord(join(f.root, "target", "budget-records")), /incomplete/);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the budget and its producer are wired where the onevcs project's budgets target reads them", () => {
  const read = path => JSON.parse(readFileSync(new URL(path, import.meta.url)));
  const crate = read("../crates/onevcs/project.json");
  const producer = read("../crates/onevcs/tests/recovery-workload/project.json");
  assert.ok(crate.targets.budgets.inputs.includes("{projectRoot}/budgets.yaml"));
  assert.ok(crate.targets.budgets.inputs.includes("{workspaceRoot}/scripts/boundary-check-budget*.mjs"));
  for (const target of ["test", "test-quick"]) {
    assert.ok(producer.targets[target].inputs.includes("{workspaceRoot}/crates/onevcs-testing/src/boundary.rs"), target);
    assert.ok(producer.targets[target].inputs.includes("{workspaceRoot}/scripts/boundary-check-budget*.mjs"), target);
  }
  const budgets = readFileSync(new URL("../crates/onevcs/budgets.yaml", import.meta.url), "utf8");
  assert.match(budgets, /id: public-boundary-check-latency/);
  assert.match(budgets, /command: \[node, \.\.\/\.\.\/scripts\/boundary-check-budget\.mjs\]/);
  const justfile = readFileSync(new URL("../justfile", import.meta.url), "utf8");
  assert.match(justfile, /check budgets\.yaml crates\/onevcs\/budgets\.yaml/);
});

// Inside a producing invocation the wrapper hands every step its id, so the record the
// journeys just wrote is held to the reader here: the journey-to-reader path, end to end.
const invocation = process.env.ONEVCS_RECOVERY_INVOCATION;
test("the record this invocation's journeys wrote is one the reader accepts", { skip: !invocation && "not inside a producing invocation" }, () => {
  const record = readBoundaryRecord(recordDirectory);
  assert.equal(record.build.run_id, invocation);
  assert.ok(record.check.total_us > 0);
});
