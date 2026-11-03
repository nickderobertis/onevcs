import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync, copyFileSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readSanitizeRecord, recordDirectory } from "./boundary-sanitize-budget-record.mjs";
import { sourceFingerprint, inputFiles, repositoryRoot } from "../../../scripts/recoverable-build.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const schema = JSON.parse(readFileSync(new URL("./boundary-sanitize-budget.schema.json", import.meta.url)));
const scratch = () => process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir();
function fixture() {
  const root = mkdtempSync(join(scratch(), "boundary-sanitize-reader-"));
  const binary = Buffer.from("the producing executable bytes");
  writeFileSync(join(root, "onevcs"), binary);
  const build = { run_id: sha("invocation"), binary: "onevcs", binary_sha256: sha(binary), source_sha256: sourceFingerprint() };
  const workload = schema["x-workload"];
  const record = {
    version: 1, build, scenario: "sanitize-call",
    covered: { identities: workload.identities, terms: workload.terms, levels: workload.levels, bytes: workload.bytes,
      metadata: workload.metadata, replaced: workload.hits, matcher_builds: 1, replacement_passes: 1, rematches: 1 },
    sanitize_wall_us: 140000,
    phases: { total_us: 90000, derivation_us: 70000, matcher_build_us: 9000, replacement_us: 400, matching_us: 100 },
    sanitize_peak_rss_kib: 20480, check_wall_us: 130000, preparation_ms: 3000, load1: 1,
  };
  const invocation = { state: "complete", ...build };
  const save = () => {
    writeFileSync(join(root, "boundary-sanitize.json"), JSON.stringify(record));
    writeFileSync(join(root, "recoverable-invocation.json"), JSON.stringify(invocation));
  };
  save();
  return { root, record, invocation, save };
}

test("reader accepts a completed matching record", () => {
  const f = fixture();
  try { assert.deepEqual(readSanitizeRecord(f.root), f.record); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

const refusals = {
  "a missing record": f => rmSync(join(f.root, "boundary-sanitize.json")),
  "a malformed record": f => writeFileSync(join(f.root, "boundary-sanitize.json"), "{broken"),
  "a failed producer": f => { f.invocation.state = "failed"; f.save(); },
  "a foreign invocation": f => { f.invocation.run_id = sha("another invocation"); f.save(); },
  "stale source inputs": f => { f.record.build.source_sha256 = sha("another source tree"); f.save(); },
  "a stale binary": f => writeFileSync(join(f.root, "onevcs"), "another build"),
  "a foreign scenario": f => { f.record.scenario = "publication-term-check"; f.save(); },
  "a shrunk scope": f => { f.record.covered.identities = 2; f.save(); },
  "a missing level": f => { f.record.covered.levels = 2; f.save(); },
  "fewer hits": f => { f.record.covered.replaced = 3; f.save(); },
  "a second matcher build": f => { f.record.covered.matcher_builds = 2; f.save(); },
  "a second replacement pass": f => { f.record.covered.replacement_passes = 2; f.save(); },
  "phases longer than the call": f => { f.record.phases.derivation_us = 200000; f.save(); },
  "an unknown field": f => { f.record.requests = 3; f.save(); },
};
for (const [name, mutate] of Object.entries(refusals)) test(`reader refuses ${name}`, () => {
  const f = fixture();
  try { mutate(f); assert.throws(() => readSanitizeRecord(f.root)); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the CLI reports the call's wall time and its phases through the SDK, and names the producer on refusal", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    symlinkSync(fileURLToPath(new URL("../../../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
    for (const path of inputFiles()) {
      mkdirSync(join(f.root, path, ".."), { recursive: true });
      copyFileSync(join(repositoryRoot, path), join(f.root, path));
    }
    for (const name of ["onevcs", "boundary-sanitize.json", "recoverable-invocation.json"]) {
      copyFileSync(join(f.root, name), join(f.root, "target", "budget-records", name));
    }
    const script = join(f.root, "crates", "onevcs", "scripts", "boundary-sanitize-budget.mjs");
    const result = join(f.root, "result.json");
    const run = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: result } });
    const accepted = run();
    assert.equal(accepted.status, 0, accepted.stderr);
    const reported = JSON.parse(readFileSync(result, "utf8"));
    assert.equal(reported.value, 0.14);
    for (const phrase of ["30 spans rewritten", "3 level files", "matcher build 0.009s (1 build)", "(1 pass)",
      "boundary check 0.130s", "peak RSS 20.0 MiB"]) {
      assert.ok(reported.detail.includes(phrase), `${phrase}: ${reported.detail}`);
    }
    const misused = run("extra");
    assert.equal(misused.status, 2);
    assert.match(misused.stderr, /next: run it as 'node scripts\/boundary-sanitize-budget\.mjs'/);
    rmSync(join(f.root, "target", "budget-records", "boundary-sanitize.json"));
    const refused = run();
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the budget and its producer are wired where the onevcs project's budgets target reads them", () => {
  const read = path => JSON.parse(readFileSync(new URL(path, import.meta.url)));
  const crate = read("../project.json");
  const producer = read("../tests/recovery-workload/project.json");
  assert.ok(crate.targets.budgets.inputs.includes("{projectRoot}/scripts/boundary-sanitize-budget*"));
  for (const target of ["test", "test-quick"]) {
    assert.ok(producer.targets[target].inputs.includes("{workspaceRoot}/crates/onevcs/scripts/boundary-sanitize-budget*"), target);
  }
  const budgets = readFileSync(new URL("../budgets.yaml", import.meta.url), "utf8");
  assert.match(budgets, /id: public-boundary-sanitize-latency/);
  assert.match(budgets, /command: \[node, scripts\/boundary-sanitize-budget\.mjs\]/);
});

// Inside a producing invocation the record the journey just wrote is held to the reader.
const invocation = process.env.ONEVCS_RECOVERY_INVOCATION;
const skip = !invocation ? "not inside a producing invocation"
  : process.platform === "win32" ? "the journey that writes the record runs on unix only" : false;
test("the record this invocation's journey wrote is one the reader accepts", { skip }, () => {
  const record = readSanitizeRecord(recordDirectory);
  assert.equal(record.build.run_id, invocation);
  assert.equal(record.covered.replaced, schema["x-workload"].hits);
});
