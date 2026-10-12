import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync, copyFileSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readDraftHoldRecord, recordDirectory } from "./draft-early-lift-budget-record.mjs";
import { sourceFingerprint, inputFiles, repositoryRoot } from "../../../scripts/recoverable-build.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const schema = JSON.parse(readFileSync(new URL("./draft-early-lift-budget.schema.json", import.meta.url)));
const scratch = () => process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir();
const scenario = (shape, activity_reads, extra = {}) => ({
  shape, outcome: "checks-unsettled", lifted_early: false, polls: 120, check_read_requests: 240,
  watch_requests: 280, activity_reads, reused_reads: 90, activity_requests: 2 * activity_reads, wall_ms: 60000, ...extra,
});
function fixture() {
  const root = mkdtempSync(join(scratch(), "draft-hold-reader-"));
  const binary = Buffer.from("the producing executable bytes");
  writeFileSync(join(root, "onevcs"), binary);
  const build = { run_id: sha("invocation"), binary: "onevcs", binary_sha256: sha(binary), source_sha256: sourceFingerprint() };
  const workload = schema["x-workload"];
  const record = {
    version: 1, build, scale: workload.scale, poll_seconds: workload.poll_seconds, grace_seconds: workload.grace_seconds,
    wait_seconds: workload.wait_seconds, reuse_seconds: workload.reuse_seconds,
    scenarios: [
      scenario("needs-gated", 3, { outcome: "merged" }),
      scenario("two-phase", 13, { lifted_early: true }),
      scenario("draft-skipping", 1, { outcome: "merged", lifted_early: true }),
      scenario("never-idle", 12),
    ],
    load1: 1,
  };
  const invocation = { state: "complete", ...build };
  const save = () => {
    writeFileSync(join(root, "draft-hold.json"), JSON.stringify(record));
    writeFileSync(join(root, "recoverable-invocation.json"), JSON.stringify(invocation));
  };
  save();
  return { root, record, invocation, save };
}

test("reader accepts a completed matching record", () => {
  const f = fixture();
  try { assert.deepEqual(readDraftHoldRecord(f.root), f.record); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

const refusals = {
  "a missing record": f => rmSync(join(f.root, "draft-hold.json")),
  "a malformed record": f => writeFileSync(join(f.root, "draft-hold.json"), "{broken"),
  "a failed producer": f => { f.invocation.state = "failed"; f.save(); },
  "a foreign invocation": f => { f.invocation.run_id = sha("another invocation"); f.save(); },
  "stale source inputs": f => { f.record.build.source_sha256 = sha("another source tree"); f.save(); },
  "a stale binary": f => writeFileSync(join(f.root, "onevcs"), "another build"),
  "a foreign format": f => { f.record.version = 2; f.save(); },
  "another poll": f => { f.record.poll_seconds = 5; f.save(); },
  "another reuse window": f => { f.record.reuse_seconds = 30; f.save(); },
  "a missing scenario": f => { f.record.scenarios[3].shape = "two-phase"; f.save(); },
  "a watch that never polled": f => { f.record.scenarios[0].polls = 0; f.save(); },
  "a scenario that never asked": f => { f.record.scenarios[1].activity_reads = 0; f.save(); },
  "an unknown field": f => { f.record.requests = 3; f.save(); },
};
for (const [name, mutate] of Object.entries(refusals)) test(`reader refuses ${name}`, () => {
  const f = fixture();
  try { mutate(f); assert.throws(() => readDraftHoldRecord(f.root)); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the CLI reports the worst workload's added requests and each watch through the SDK, and names the producer on refusal", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    symlinkSync(fileURLToPath(new URL("../../../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
    for (const path of inputFiles()) {
      mkdirSync(join(f.root, path, ".."), { recursive: true });
      copyFileSync(join(repositoryRoot, path), join(f.root, path));
    }
    for (const name of ["onevcs", "draft-hold.json", "recoverable-invocation.json"]) {
      copyFileSync(join(f.root, name), join(f.root, "target", "budget-records", name));
    }
    const result = join(f.root, "result.json");
    const run = () => spawnSync(process.execPath, [join(f.root, "crates", "onevcs", "scripts", "draft-early-lift-budget.mjs")], {
      encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: result },
    });
    const accepted = run();
    assert.equal(accepted.status, 0, accepted.stderr);
    const reported = JSON.parse(readFileSync(result, "utf8"));
    assert.equal(reported.value, 26, "the two-phase watch's 13 reads at two requests each");
    for (const phrase of ["worst is two-phase", "needs-gated (merged): 120 polls", "2.0 check-read requests a poll",
      "280 requests in the whole watch", "13 head-activity reads", "90 polls answered from the reuse", "2.0 requests a read"]) {
      assert.ok(reported.detail.includes(phrase), `${phrase}: ${reported.detail}`);
    }
    const misused = spawnSync(process.execPath, [join(f.root, "crates", "onevcs", "scripts", "draft-early-lift-budget.mjs"), "extra"], {
      encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: result },
    });
    assert.equal(misused.status, 2);
    assert.match(misused.stderr, /expected no arguments, got extra/);
    assert.match(misused.stderr, /next: run it as 'node scripts\/draft-early-lift-budget\.mjs'/);
    rmSync(join(f.root, "target", "budget-records", "draft-hold.json"));
    const refused = run();
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /draft hold telemetry: /);
    assert.match(refused.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the budget and its producer are wired where the onevcs project's budgets target reads them", () => {
  const read = path => JSON.parse(readFileSync(new URL(path, import.meta.url)));
  const crate = read("../project.json");
  const producer = read("../tests/recovery-workload/project.json");
  assert.ok(crate.targets.budgets.inputs.includes("{projectRoot}/scripts/draft-early-lift-budget*"));
  for (const target of ["test", "test-quick"]) {
    assert.ok(producer.targets[target].inputs.includes("{workspaceRoot}/crates/onevcs/scripts/draft-early-lift-budget*"), target);
    assert.ok(producer.targets[target].inputs.includes("{workspaceRoot}/crates/onevcs/tests/fixtures/gh"), target);
  }
  const budgets = readFileSync(new URL("../budgets.yaml", import.meta.url), "utf8");
  assert.match(budgets, /id: draft-early-lift-added-host-requests/);
  assert.match(budgets, /command: \[node, scripts\/draft-early-lift-budget\.mjs\]/);
});

// Inside a producing invocation the record the journey just wrote is held to the reader.
const invocation = process.env.ONEVCS_RECOVERY_INVOCATION;
const skip = !invocation ? "not inside a producing invocation"
  : process.platform === "win32" ? "the journey that writes the record runs on unix only" : false;
test("the record this invocation's journey wrote is one the reader accepts", { skip }, () => {
  const record = readDraftHoldRecord(recordDirectory);
  assert.equal(record.build.run_id, invocation);
  assert.ok(record.scenarios.every(scenario => scenario.polls > 0));
});
