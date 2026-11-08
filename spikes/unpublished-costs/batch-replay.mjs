#!/usr/bin/env node
// The batched-read strategy for the spawned git reads one read makes, measured by replay.
// llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate] a one-off replay of git's own cat-file shape; this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree.
//
//   node spikes/unpublished-costs/batch-replay.mjs --binary ENGINE --acknowledgements DIR
//        [--onevcs-home DIR] [--shape cat-file-e]
//
// One untimed counting call runs `unpublished --host` with a logging `git` shim first on
// PATH, which records each spawned git's working directory and argv and then runs the real
// git. The calls of the named shape are then replayed twice, never alongside the read:
// once as they were made, one process each, and once as one `git cat-file --batch-check`
// per checkout with every object on standard input — the batch a per-checkout reader
// would make. Prints one JSON document: the calls, the checkouts, both wall times, the
// load, and whether every answer agreed.

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const given = {};
for (let at = 2; at < process.argv.length; at += 2) {
  const [name, value] = [process.argv[at], process.argv[at + 1]];
  if (!["--binary", "--acknowledgements", "--onevcs-home", "--shape"].includes(name) || value === undefined || value.startsWith("--")) {
    process.stderr.write(`batch-replay: ${JSON.stringify(name)} is not --binary, --acknowledgements, --onevcs-home or --shape, or has no value\n`);
    process.exit(2);
  }
  given[name] = value;
}
const flag = (name) => given[name];
const binary = flag("--binary");
const acknowledgements = flag("--acknowledgements");
if (!binary || !acknowledgements) {
  process.stderr.write("batch-replay: --binary ENGINE --acknowledgements DIR are required\n");
  process.exit(2);
}
const refuse = (message) => {
  process.stderr.write(`batch-replay: ${message}\n`);
  process.exit(1);
};
const shape = flag("--shape") || "cat-file-e";
if (shape !== "cat-file-e") {
  process.stderr.write(`batch-replay: only --shape cat-file-e is replayed, not ${shape}\n`);
  process.exit(2);
}
const scratch = fs.mkdtempSync(path.join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || os.tmpdir(), "replay-"));
const found = spawnSync("sh", ["-c", "command -v git"], { encoding: "utf8" });
if (found.status !== 0 || !found.stdout.trim()) refuse(`no git on PATH to replay with: ${found.stderr.trim() || `status ${found.status}`}`);
const real = found.stdout.trim();
const log = path.join(scratch, "calls.ndjson");
const shim = path.join(scratch, "bin");
fs.mkdirSync(shim);
fs.writeFileSync(
  path.join(shim, "git"),
  `#!/usr/bin/env node
const fs = require("node:fs");
fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ cwd: process.cwd(), argv: process.argv.slice(2) }) + "\\n");
const ran = require("node:child_process").spawnSync(${JSON.stringify(real)}, process.argv.slice(2), { stdio: "inherit" });
process.exit(ran.status === null ? 1 : ran.status);
`,
  { mode: 0o755 },
);
const env = { ...process.env, PATH: `${shim}:${process.env.PATH}` };
delete env.ONEVCS_SPIKE_TRACE;
delete env.ONEVCS_SPIKE_PROTOTYPE;
if (flag("--onevcs-home")) env.ONEVCS_HOME = path.resolve(flag("--onevcs-home"));
const counted = spawnSync(binary, ["unpublished", "--host", "--format", "json", "--acknowledgements", acknowledgements], {
  env,
  cwd: scratch,
  encoding: "utf8",
  maxBuffer: 1 << 30,
});
// The read's own answers are 0 (none owed), 7 (owed) and 1 (unanswered); anything else,
// or a read that never started, made no calls worth replaying.
if (counted.error || ![0, 1, 7].includes(counted.status))
  refuse(`the counting read did not run to an answer: ${counted.error?.message || `exit ${counted.status}: ${counted.stderr.trim().split("\n").at(-1)}`}`);
const calls = fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)) : [];
// `git [-C dir] cat-file -e <object>`, wherever the directory was given.
const located = calls
  .map((call) => {
    let { cwd, argv } = call;
    while (argv[0] === "-C") {
      cwd = path.resolve(cwd, argv[1]);
      argv = argv.slice(2);
    }
    return { cwd, argv };
  })
  .filter((call) => call.argv[0] === "cat-file" && call.argv[1] === "-e" && call.argv.length === 3);
const load = () => os.loadavg().map((value) => Math.round(value * 100) / 100);
const clock = () => Number(process.hrtime.bigint()) / 1e9;

// llmlint: ignore[changed_behavior_has_e2e] refusing a read that spawned no cat-file -e needs a read of a host with no such calls, and every checkout this harness generates spawns them; the journey drives a replay and a missing engine, and this measurement code lives only on the preserved spike branch, which the plan's run discards.
if (!located.length) refuse(`the read spawned ${calls.length} git processes and none was \`cat-file -e <object>\`, so there is nothing to replay`);
const loadBefore = load();
let began = clock();
// llmlint: ignore[changed_behavior_has_e2e] a replay's git failing mid-run is a fault of the host being read, refused loudly here; this measurement code lives only on the preserved spike branch, which the plan's run discards, and harness.test.mjs drives the replay's success and its missing-engine failure.
const one = located.map((call) => {
  const ran = spawnSync(real, call.argv, { cwd: call.cwd });
  // A replay that failed to run is no answer about the object; cat-file -e answers 0 or 1.
  if (ran.error || ![0, 1].includes(ran.status)) refuse(`git cat-file -e did not answer in a checkout: ${ran.error?.message ?? `status ${ran.status}`}`);
  // llmlint: ignore[changed_behavior_has_e2e] an absent object needs a read naming one, and the generated hosts name only objects they hold; a real read's missing object is the same path, its answer compared with the batch's line for line; this measurement code lives only on the preserved spike branch, which the plan's run discards.
  return ran.status === 0;
});
const oneEach = clock() - began;

const byCheckout = new Map();
located.forEach((call, at) => {
  const list = byCheckout.get(call.cwd) || [];
  list.push({ object: call.argv[2], at });
  byCheckout.set(call.cwd, list);
});
const batched = new Array(located.length);
began = clock();
for (const [cwd, list] of byCheckout) {
  const ran = spawnSync(real, ["cat-file", "--batch-check=%(objectname)"], {
    cwd,
    input: list.map((entry) => entry.object).join("\n") + "\n",
    encoding: "utf8",
    maxBuffer: 1 << 28,
  });
  // llmlint: ignore[changed_behavior_has_e2e] these refusals are of a git process failing on the host being read, which no generated checkout can arrange on demand; each is written to refuse rather than answer, and this measurement code lives only on the preserved spike branch, which the plan's run discards.
  if (ran.error || ran.status !== 0) refuse(`git cat-file --batch-check in ${cwd} failed: ${ran.error?.message || ran.stderr.trim()}`);
  const out = ran.stdout.split("\n").filter(Boolean);
  // One line per object asked, each the object's name or `<object> missing`.
  // llmlint: ignore[changed_behavior_has_e2e] git cat-file answers one line per object; wrong counts or odd lines are a broken git, which no generated checkout can arrange, and this refuses rather than answers; this measurement code lives only on the preserved spike branch, which the plan's run discards.
  if (out.length !== list.length) refuse(`git cat-file --batch-check in ${cwd} answered ${out.length} lines for ${list.length} objects`);
  list.forEach((entry, index) => {
    if (out[index].endsWith(" missing")) batched[entry.at] = false;
    else if (/^[0-9a-f]{40,64}$/.test(out[index])) batched[entry.at] = true;
    // llmlint: ignore[changed_behavior_has_e2e] as above, a line that names no object is a broken git and is refused; this measurement code lives only on the preserved spike branch.
    else refuse(`git cat-file --batch-check in ${cwd} answered ${JSON.stringify(out[index])}, which names no object`);
  });
}
const batch = clock() - began;
const round = (seconds) => Math.round(seconds * 1000) / 1000;
const agree = one.every((answer, at) => answer === batched[at]);
process.stdout.write(
  `${JSON.stringify(
    {
      measure: "batch-replay",
      shape,
      read_status: counted.status,
      spawned_git: calls.length,
      calls: located.length,
      checkouts: byCheckout.size,
      one_process_each_seconds: round(oneEach),
      one_batch_per_checkout_seconds: round(batch),
      answers_agree: agree,
      load_before: loadBefore,
      load_after: load(),
    },
    null,
    1,
  )}\n`,
);
// llmlint: ignore[changed_behavior_has_e2e] no generated checkout can make a batched and a spawned cat-file disagree; a disagreement exits non-zero here rather than reading as a result, and this measurement code lives only on the preserved spike branch, which the plan's run discards.
if (!agree) refuse("one process per question and one batch per checkout answered differently; the document above says where");
