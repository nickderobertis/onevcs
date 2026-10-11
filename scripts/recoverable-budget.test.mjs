import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync, copyFileSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readRecord } from "./recoverable-budget-record.mjs";
import { sourceFingerprint, inputFiles, repositoryRoot } from "./recoverable-build.mjs";

const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const schema = JSON.parse(readFileSync(new URL("./recoverable-budget.schema.json",import.meta.url)));
function fixture() {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(),"recovery-reader-"));
  const binary = Buffer.from("the producing executable bytes");
  writeFileSync(join(root,"onevcs"),binary);
  const build = { run_id:sha("invocation"), binary:"onevcs",binary_sha256:sha(binary),source_sha256:sourceFingerprint() };
  const sample = {wall_ms:10,load1:1,rows:5,verdict_sha256:sha("validated verdict")};
  const class_counts = Object.fromEntries(["in-part","landed","live","no","retirable","superseded","unknown"].map(name=>[name,1]));
  const call = {wall_ms:12.5,load1:1,rows:5,verdict_sha256:sha("uncached verdict")};
  const calls = () => Array.from({length:10},()=>({...call}));
  const record = {version:2,build,scenario:"launcher-decision",preparation_ms:10,total_ms:20,load1:1,
    workloads:[1,10].map(scale=>({scale,shape:schema["x-workloads"][String(scale)],class_counts,cold:sample,warm:sample,cold_git:100,warm_git:10,counted_verdict_sha256:sample.verdict_sha256,
      uncached_verdict_sha256:call.verdict_sha256,in_process_warm:calls(),...(scale === 1 ? {in_process_cold:calls()} : {})}))};
  const invocation = {state:"complete",...build};
  const save = () => {
    writeFileSync(join(root,"recoverable.json"),JSON.stringify(record));
    writeFileSync(join(root,"recoverable-invocation.json"),JSON.stringify(invocation));
  };
  save();
  return {root,record,invocation,save};
}

test("reader accepts a completed matching artifact and its portable cache restoration",()=>{
  const f = fixture();
  const restored = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(),"recovery-restored-"));
  try {
    assert.deepEqual(readRecord(f.root),f.record);
    for (const name of ["onevcs","recoverable.json","recoverable-invocation.json"]) copyFileSync(join(f.root,name),join(restored,name));
    assert.deepEqual(readRecord(restored),f.record);
  } finally { rmSync(f.root,{recursive:true,force:true});rmSync(restored,{recursive:true,force:true}); }
});
const refusals = {
  "missing record":f=>rmSync(join(f.root,"recoverable.json")),
  "unreadable record":f=>{rmSync(join(f.root,"recoverable.json"));mkdirSync(join(f.root,"recoverable.json"));},
  "malformed record":f=>writeFileSync(join(f.root,"recoverable.json"),"{broken"),
  "missing invocation":f=>rmSync(join(f.root,"recoverable-invocation.json")),
  "failed producer":f=>{f.invocation.state="failed";f.save();},
  "skipped or incomplete producer":f=>{f.invocation.state="started";f.save();},
  "foreign invocation":f=>{f.invocation.run_id=sha("another invocation");f.save();},
  "merely nonempty run id":f=>{f.invocation.run_id=f.record.build.run_id="anything";f.save();},
  "stale source inputs":f=>{f.record.build.source_sha256=sha("another source tree");f.save();},
  "stale binary":f=>writeFileSync(join(f.root,"onevcs"),"another build"),
  "foreign binary path":f=>{f.record.build.binary=f.invocation.binary="../other";f.save();},
  "foreign scenario":f=>{f.record.scenario="other";f.save();},
  "foreign format":f=>{f.record.version=1;f.save();},
  "missing in-process calls":f=>{delete f.record.workloads[1].in_process_warm;f.save();},
  "missing in-process cold calls":f=>{delete f.record.workloads[0].in_process_cold;f.save();},
  "too few in-process calls":f=>{f.record.workloads[0].in_process_cold.pop();f.save();},
  "an in-process call answering other rows":f=>{f.record.workloads[0].in_process_warm[3].verdict_sha256=sha("different verdict");f.save();},
  "an untimed in-process call":f=>{f.record.workloads[1].in_process_warm[9].wall_ms=0;f.save();},
  "foreign scale":f=>{f.record.workloads[0].scale=10;f.save();},
  "shrunk workload":f=>{f.record.workloads[0].shape=[1,1,1,1];f.save();},
  "missing sample":f=>{delete f.record.workloads[0].warm;f.save();},
  "wrongly empty read":f=>{f.record.workloads[0].cold.rows=0;f.save();},
  "missing class":f=>{delete f.record.workloads[0].class_counts.unknown;f.save();},
  "missing actual count":f=>{f.record.workloads[0].cold_git=0;f.save();},
  "different counted answer":f=>{f.record.workloads[0].counted_verdict_sha256=sha("different verdict");f.save();},
  "missing preparation":f=>{f.record.preparation_ms=0;f.save();},
  "incomplete total":f=>{f.record.total_ms=1;f.save();},
};
for (const [name,mutate] of Object.entries(refusals)) test(`reader refuses ${name}`,()=>{
  const f = fixture();
  try { mutate(f);assert.throws(()=>readRecord(f.root)); }
  finally {rmSync(f.root,{recursive:true,force:true});}
});

// Drive the shipped reader in a disposable installation: it can report with PATH
// empty, and every refusal names the producing command rather than doing work.
test("CLI reader reports through the SDK without subprocess tools and names recovery on refusal", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    symlinkSync(fileURLToPath(new URL("../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
    for (const path of inputFiles()) {
      mkdirSync(join(f.root, path, ".."), {recursive:true});
      copyFileSync(join(repositoryRoot,path),join(f.root,path));
    }
    for (const name of ["onevcs", "recoverable.json", "recoverable-invocation.json"]) {
      copyFileSync(join(f.root, name), join(f.root, "target", "budget-records", name));
    }
    const run = () => spawnSync(process.execPath, [join(f.root, "scripts", "recoverable-git-count-budget.mjs"), "--scale", "1", "--warm"], {
      encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: join(f.root, "result.json") }
    });
    const accepted = run();
    assert.equal(accepted.status, 0, accepted.stderr);
    assert.ok(readFileSync(join(f.root, "result.json")).length > 0);
    rmSync(join(f.root, "target", "budget-records", "recoverable.json"));
    const refused = run();
    assert.notEqual(refused.status, 0);
    assert.match(refused.stderr, /next: run 'just recoverable-journeys'/);
    rmSync(join(f.root, "scripts", "recoverable-budget.schema.json"));
    const unschemed = run();
    assert.equal(unschemed.status, 1);
    assert.match(unschemed.stderr, /recoverable telemetry: .*recoverable-budget\.schema\.json/);
    assert.match(unschemed.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root, {recursive:true,force:true}); }
});

// The latency reader reports the slowest of the ten recorded calls, in seconds, and
// refuses a mode the record does not hold rather than reporting nothing.
test("latency reader reports the slowest in-process call and refuses unrecorded modes", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    mkdirSync(join(f.root, "target", "budget-records"), { recursive: true });
    symlinkSync(fileURLToPath(new URL("../node_modules", import.meta.url)), join(f.root, "node_modules"), "dir");
    for (const path of inputFiles()) {
      mkdirSync(join(f.root, path, ".."), {recursive:true});
      copyFileSync(join(repositoryRoot,path),join(f.root,path));
    }
    f.record.workloads[1].in_process_warm[4].wall_ms = 270.25;
    f.save();
    for (const name of ["onevcs", "recoverable.json", "recoverable-invocation.json"]) {
      copyFileSync(join(f.root, name), join(f.root, "target", "budget-records", name));
    }
    const run = (...args) => spawnSync(process.execPath, [join(f.root, "scripts", "recoverable-latency-budget.mjs"), ...args], {
      encoding: "utf8", env: { PATH: "", ONEBUDGETSPEC_RESULT: join(f.root, "result.json") }
    });
    for (const [args, value] of [[["--scale", "1", "--warm"], 0.0125], [["--scale", "1", "--cold"], 0.0125], [["--scale", "10", "--warm"], 0.27025]]) {
      rmSync(join(f.root, "result.json"), { force: true });
      const accepted = run(...args);
      assert.equal(accepted.status, 0, accepted.stderr);
      assert.ok(readFileSync(join(f.root, "result.json"), "utf8").includes(String(value)), `${args.join(" ")} reports ${value}`);
    }
    const unrecorded = run("--scale", "10", "--cold");
    assert.equal(unrecorded.status, 2);
    assert.match(unrecorded.stderr, /in-process cold calls are recorded at --scale 1 only/);
    assert.match(unrecorded.stderr, /next: pass --scale 1 --warm, --scale 10 --warm or --scale 1 --cold/);
    const malformed = run("--scale", "2", "--warm");
    assert.equal(malformed.status, 2);
    assert.match(malformed.stderr, /expected --scale 1\|10 --cold\|--warm/);
    assert.match(malformed.stderr, /next: pass --scale 1 --warm/);
    // Run outside onebudgetspec it has nowhere to report, and names the recipe that
    // gives it somewhere.
    const unwired = spawnSync(process.execPath, [join(f.root, "scripts", "recoverable-latency-budget.mjs"), "--scale", "1", "--warm"], {
      encoding: "utf8", env: { PATH: "" }
    });
    assert.equal(unwired.status, 2);
    assert.match(unwired.stderr, /ONEBUDGETSPEC_RESULT is missing/);
    assert.match(unwired.stderr, /next: run 'just budgets'/);
    // A stale or missing record is the telemetry's to regenerate, and says so.
    rmSync(join(f.root, "target", "budget-records", "recoverable.json"));
    const stale = run("--scale", "1", "--warm");
    assert.equal(stale.status, 1);
    assert.match(stale.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root, {recursive:true,force:true}); }
});

test("Nx budgets follow the single owning producer and restore its complete provenance", () => {
  const read = path => JSON.parse(readFileSync(new URL(path, import.meta.url)));
  const crate = read("../crates/onevcs/project.json");
  const producer = read("../crates/onevcs/tests/recovery-workload/project.json");
  assert.deepEqual(crate.targets.budgets.dependsOn, [{target:"test",projects:[producer.name]}]);
  assert.ok(crate.targets.check.dependsOn.includes("budgets"));
  assert.equal(producer.targets.test.command,"just _recovery-test");
  assert.ok(producer.targets.test.outputs.includes("{workspaceRoot}/target/budget-records"));
  assert.ok(producer.targets.test.inputs.includes("{workspaceRoot}/crates/onevcs/src/**/*"));
  assert.ok(!crate.targets.budgets.dependsOn.some(edge=>edge.projects?.includes("onevcs-e2e")));
});

test("a failed producer invalidates prior records before its command starts", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    mkdirSync(join(f.root, "target", "budget-records"), {recursive:true});
    copyFileSync(fileURLToPath(new URL("recoverable-invocation.mjs", import.meta.url)), join(f.root, "scripts", "recoverable-invocation.mjs"));
    copyFileSync(join(f.root,"recoverable.json"),join(f.root,"target","budget-records","recoverable.json"));
    const run = spawnSync(process.execPath, [join(f.root,"scripts","recoverable-invocation.mjs"), process.execPath, "-e",
      "const fs=require('node:fs'); const p='target/budget-records/'; if(fs.existsSync(p+'recoverable.json'))process.exit(2); const m=JSON.parse(fs.readFileSync(p+'recoverable-invocation.json')); if(m.state!=='started'||m.run_id!==process.env.ONEVCS_RECOVERY_INVOCATION)process.exit(3); process.exit(1)"],
      {cwd:f.root,encoding:"utf8"});
    assert.equal(run.status,1,run.stderr);
    assert.match(run.stderr, /failed with exit 1/);
    assert.match(run.stderr, /next: run 'just recoverable-journeys'/);
    const manifest = JSON.parse(readFileSync(join(f.root,"target","budget-records","recoverable-invocation.json")));
    assert.equal(manifest.state,"failed");
    assert.throws(()=>readFileSync(join(f.root,"target","budget-records","recoverable.json")));
    assert.throws(()=>readRecord(join(f.root,"target","budget-records")), /incomplete/);
  } finally {rmSync(f.root,{recursive:true,force:true});}
});

test("a missing producer command refuses with its cause and regeneration command", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    copyFileSync(fileURLToPath(new URL("recoverable-invocation.mjs", import.meta.url)), join(f.root, "scripts", "recoverable-invocation.mjs"));
    const run = spawnSync(process.execPath, [join(f.root, "scripts", "recoverable-invocation.mjs")], {cwd:f.root,encoding:"utf8"});
    assert.equal(run.status,2);
    assert.match(run.stderr, /producer command is missing/);
    assert.match(run.stderr, /next: run 'just recoverable-journeys'/);
    assert.equal(JSON.parse(readFileSync(join(f.root,"target/budget-records/recoverable-invocation.json"))).state,"failed");
  } finally { rmSync(f.root,{recursive:true,force:true}); }
});

test("reader rejects changed current source even when artifact and binary still match", () => {
  const f = fixture();
  const sources = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(),"recovery-sources-"));
  try {
    for (const path of inputFiles()) {
      mkdirSync(join(sources,path,".."),{recursive:true});
      copyFileSync(join(repositoryRoot,path),join(sources,path));
    }
    assert.deepEqual(readRecord(f.root,sources),f.record);
    writeFileSync(join(sources,"crates/onevcs/src/lib.rs"),"changed current build input");
    assert.throws(()=>readRecord(f.root,sources),/stale source\/build/);
  } finally {rmSync(f.root,{recursive:true,force:true});rmSync(sources,{recursive:true,force:true});}
});

test("a producer that cannot reset its records refuses with the cause and regeneration command", () => {
  const f = fixture();
  try {
    mkdirSync(join(f.root, "scripts"));
    copyFileSync(fileURLToPath(new URL("recoverable-invocation.mjs", import.meta.url)), join(f.root, "scripts", "recoverable-invocation.mjs"));
    writeFileSync(join(f.root, "target"), "a file where the records directory belongs");
    const run = spawnSync(process.execPath, [join(f.root, "scripts", "recoverable-invocation.mjs"), process.execPath, "-e", "process.exit(0)"], {cwd:f.root,encoding:"utf8"});
    assert.equal(run.status, 1);
    assert.match(run.stderr, /could not reset the records under .*budget-records/);
    assert.match(run.stderr, /next: run 'just recoverable-journeys'/);
  } finally { rmSync(f.root,{recursive:true,force:true}); }
});

const manifestRefusals = {
  "a parent-escaping file": [{files:["../outside"],directories:[],prefixes:[]}, /files entry "\.\.\/outside" is not a repository-relative path/],
  "an absolute directory": [{files:[],directories:["/etc"],prefixes:[]}, /directories entry "\/etc" is not a repository-relative path/],
  "a non-string file": [{files:[7],directories:[],prefixes:[]}, /files entry 7 is not a repository-relative path/],
  "files that are not a list": [{files:"Cargo.toml",directories:[],prefixes:[]}, /files is not a list/],
  "a prefix without a directory": [{files:[],directories:[],prefixes:[{prefix:"recoverable"}]}, /prefixes entry undefined is not a repository-relative path/],
  "a prefix naming a path": [{files:[],directories:[],prefixes:[{directory:"scripts",prefix:"../x"}]}, /needs a non-empty prefix without '\/'/],
};
for (const [name,[manifest,message]] of Object.entries(manifestRefusals)) test(`build-input manifest refuses ${name}`,()=>{
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || tmpdir(),"recovery-manifest-"));
  try {
    mkdirSync(join(root,"scripts"));
    writeFileSync(join(root,"scripts","recoverable-build-inputs.json"),JSON.stringify({version:1,...manifest}));
    assert.throws(()=>inputFiles(root),message);
  } finally { rmSync(root,{recursive:true,force:true}); }
});
