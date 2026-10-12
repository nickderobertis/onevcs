import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync, copyFileSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readRetireHourRecord, recordDirectory } from "./retire-hour-host-requests-budget-record.mjs";
import { sourceFingerprint, inputFiles, repositoryRoot } from "../../../scripts/recoverable-build.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const schema = JSON.parse(readFileSync(new URL("./retire-hour-host-requests-budget.schema.json", import.meta.url)));
const scratch = () => process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir();
const run = (kind, extra = {}) => ({
  kind, requests: 36, graphql_points: 36, rest_requests: 0, examined: 19, from_records: 0, asked: 18,
  merged_views: 18, open_lists: 18, merge_times: 0, other: 0, retired: 0, wall_ms: 4000, ...extra,
});
function fixture() {
  const root = mkdtempSync(join(scratch(), "retire-hour-reader-"));
  const binary = Buffer.from("the producing executable bytes");
  writeFileSync(join(root, "onevcs"), binary);
  const build = { run_id: sha("invocation"), binary: "onevcs", binary_sha256: sha(binary), source_sha256: sourceFingerprint() };
  const workload = schema["x-workload"];
  const runs = [
    run("pass", { requests: 38, graphql_points: 38, asked: 19, merged_views: 19, merge_times: 1, retired: 1 }),
    ...Array.from({ length: workload.passes - 1 }, () => run("pass", { examined: 18 })),
    ...Array.from({ length: workload.sweeps }, () => run("sweep", { examined: 18 })),
  ];
  const record = {
    version: 1, build, identities: workload.identities, runs,
    requests: runs.reduce((sum, entry) => sum + entry.requests, 0), load1: 1,
  };
  const invocation = { state: "complete", ...build };
  const save = () => {
    writeFileSync(join(root, "retire-hour.json"), JSON.stringify(record));
    writeFileSync(join(root, "recoverable-invocation.json"), JSON.stringify(invocation));
  };
  save();
  return { root, record, invocation, save };
}

test("reader accepts a completed matching record", () => {
  const f = fixture();
  try { assert.deepEqual(readRetireHourRecord(f.root), f.record); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

const refusals = {
  "a missing record": f => rmSync(join(f.root, "retire-hour.json")),
  "a malformed record": f => writeFileSync(join(f.root, "retire-hour.json"), "{broken"),
  "a failed producer": f => { f.invocation.state = "failed"; f.save(); },
  "a foreign invocation": f => { f.invocation.run_id = sha("another invocation"); f.save(); },
  "stale source inputs": f => { f.record.build.source_sha256 = sha("another source tree"); f.save(); },
  "a stale binary": f => writeFileSync(join(f.root, "onevcs"), "another build"),
  "a foreign format": f => { f.record.version = 2; f.save(); },
  "another host": f => { f.record.identities = 3; f.save(); },
  "a missing pass": f => { f.record.runs.splice(1, 1); f.record.requests -= 36; f.save(); },
  "the sweeps first": f => { f.record.runs.reverse(); f.save(); },
  "a run whose kinds do not add up": f => { f.record.runs[2].other = 5; f.save(); },
  "a run whose charges do not add up": f => { f.record.runs[2].rest_requests = 4; f.save(); },
  "a total that is not the runs' sum": f => { f.record.requests = 1; f.save(); },
  "an unknown field": f => { f.record.passes = 18; f.save(); },
};
for (const [name, mutate] of Object.entries(refusals)) test(`reader refuses ${name}`, () => {
  const f = fixture();
  try { mutate(f); assert.throws(() => readRetireHourRecord(f.root)); }
  finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the CLI reports the hour's requests and each pass and sweep through the SDK, and names the producer on refusal", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    symlinkSync(fileURLToPath(new URL("../../../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
    for (const path of inputFiles()) {
      mkdirSync(join(f.root, path, ".."), { recursive: true });
      copyFileSync(join(repositoryRoot, path), join(f.root, path));
    }
    for (const name of ["onevcs", "retire-hour.json", "recoverable-invocation.json"]) {
      copyFileSync(join(f.root, name), join(f.root, "target", "budget-records", name));
    }
    const script = join(f.root, "crates", "onevcs", "scripts", "retire-hour-host-requests-budget.mjs");
    const result = join(f.root, "result.json");
    const run = () => spawnSync(process.execPath, [script], { encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: result } });
    const accepted = run();
    assert.equal(accepted.status, 0, accepted.stderr);
    const reported = JSON.parse(readFileSync(result, "utf8"));
    assert.equal(reported.value, 38 + 17 * 36 + 2 * 36, "every run's requests, summed");
    for (const phrase of ["19 identities, 18 passes and 2 sweeps", "pass 1: 38 requests (19 merged-state views, 18 open lists, 1 merge times",
      "19 branches examined, 19 asked about", "1 retired", "sweep 2: 36 requests", "by kind over the hour: 361 merged-state views"]) {
      assert.ok(reported.detail.includes(phrase), `${phrase}: ${reported.detail}`);
    }
    const misused = spawnSync(process.execPath, [script, "extra"], { encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: result } });
    assert.equal(misused.status, 2);
    assert.match(misused.stderr, /expected no arguments, got extra/);
    rmSync(join(f.root, "target", "budget-records", "retire-hour.json"));
    const refused = run();
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /retirement hour telemetry: /);
    assert.match(refused.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root, { recursive: true, force: true }); }
});

test("the budget and its producer are wired where the onevcs project's budgets target reads them", () => {
  const read = path => JSON.parse(readFileSync(new URL(path, import.meta.url)));
  const crate = read("../project.json");
  const producer = read("../tests/recovery-workload/project.json");
  assert.ok(crate.targets.budgets.inputs.includes("{projectRoot}/scripts/retire-hour-host-requests-budget*"));
  for (const target of ["test", "test-quick"]) {
    assert.ok(producer.targets[target].inputs.includes("{workspaceRoot}/crates/onevcs/scripts/retire-hour-host-requests-budget*"), target);
  }
  const budgets = readFileSync(new URL("../budgets.yaml", import.meta.url), "utf8");
  assert.match(budgets, /id: retire-hour-host-requests/);
  assert.match(budgets, /command: \[node, scripts\/retire-hour-host-requests-budget\.mjs\]/);
});

// Inside a producing invocation the record the journey just wrote is held to the reader.
const invocation = process.env.ONEVCS_RECOVERY_INVOCATION;
const skip = !invocation ? "not inside a producing invocation"
  : process.platform === "win32" ? "the journey that writes the record runs on unix only" : false;
test("the record this invocation's journey wrote is one the reader accepts", { skip }, () => {
  const record = readRetireHourRecord(recordDirectory);
  assert.equal(record.build.run_id, invocation);
  assert.ok(record.requests <= 760, `${record.requests} requests`);
});
