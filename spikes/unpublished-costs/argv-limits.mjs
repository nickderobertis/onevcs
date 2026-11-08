#!/usr/bin/env node
// For each git invocation onevcs's recovery, landing and diff layer builds whose
// argument list grows with paths, refs or commits, find the size at which spawning
// it is refused E2BIG, on a generated repository, with the argv onevcs builds.
//
//   node spikes/unpublished-costs/argv-limits.mjs [--dir DIR]
//
// Prints one JSON document: per shape, the element size used, the largest count that
// started and the smallest that was refused, and the refusal git's caller saw. Also
// says whether `git diff` accepts --pathspec-from-file, and whether `rev-list --stdin`
// takes the same list a refused argv held.

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const at = process.argv.indexOf("--dir");
const root = fs.mkdtempSync(
  path.join(at > 0 ? process.argv[at + 1] : process.env.ONEPIPELINE_NODE_SCRATCH_DIR || os.tmpdir(), "argv-limits-"),
);
const repo = path.join(root, "repo");
const git = (args, options = {}) =>
  spawnSync("git", args, { cwd: repo, encoding: "utf8", maxBuffer: 1 << 28, ...options });
fs.mkdirSync(repo);
git(["init", "-q", "-b", "main"]);
const id = { GIT_AUTHOR_NAME: "x", GIT_AUTHOR_EMAIL: "x@x", GIT_COMMITTER_NAME: "x", GIT_COMMITTER_EMAIL: "x@x" };
// Two commits and a chain of tips, built with fast-import so the ids are real.
let stream = "blob\nmark :1\ndata 2\na\n\n";
stream += "commit refs/heads/main\nmark :2\ncommitter x <x@x> 1700000000 +0000\ndata 4\nbase\nM 100644 :1 a.txt\n\n";
stream += "commit refs/heads/main\nmark :3\ncommitter x <x@x> 1700000001 +0000\ndata 4\ntip!\nfrom :2\nM 100644 :1 b.txt\n\n";
const TIPS = 60_000;
for (let n = 0; n < TIPS; n += 1) {
  stream += `commit refs/remotes/origin/t${n}\nmark :${n + 10}\ncommitter x <x@x> 1700000002 +0000\ndata 1\nt\nfrom :${n === 0 ? 2 : n + 9}\n\n`;
}
const imported = git(["fast-import", "--quiet", "--export-marks=marks"], { input: stream, env: { ...process.env, ...id } });
if (imported.status !== 0) throw new Error(imported.stderr);
const marks = new Map(
  fs
    .readFileSync(path.join(repo, "marks"), "utf8")
    .trim()
    .split("\n")
    .map((line) => line.slice(1).split(" ")),
);
const [base, tip] = [marks.get("2"), marks.get("3")];
const tips = Array.from({ length: TIPS }, (_, n) => marks.get(String(n + 10)));
const path100 = (n) => `:(literal)dir/${String(n).padStart(6, "0")}-${"p".repeat(78)}.txt`;

const shapes = {
  "known_to_carry_changes: diff --quiet A B -- <:(literal)path>…": (n) => [
    "diff", "--quiet", tip, base, "--", ...Array.from({ length: n }, (_, i) => path100(i)),
  ],
  "differing_among: diff --name-only --no-renames -z A B -- <:(literal)path>…": (n) => [
    "diff", "--name-only", "--no-renames", "-z", tip, base, "--", ...Array.from({ length: n }, (_, i) => path100(i)),
  ],
  "unpublished_ahead: rev-list --count T --not <origin tip>… --": (n) => [
    "rev-list", "--count", tip, "--not", ...tips.slice(0, n), "--",
  ],
  "unpublished_heads: for-each-ref --format=%(refname:short) --no-merged=<tip>… refs/heads": (n) => [
    "for-each-ref", "--format=%(refname:short)", ...tips.slice(0, n).map((t) => `--no-merged=${t}`), "refs/heads",
  ],
  "commits_with_parents: rev-list --parents T --not <tip>… --": (n) => [
    "rev-list", "--parents", tip, "--not", ...tips.slice(0, n), "--",
  ],
};

const bytes = (argv) => argv.reduce((total, arg) => total + Buffer.byteLength(arg) + 1, 0);
const starts = (argv) => {
  const ran = spawnSync("git", argv, { cwd: repo, encoding: "utf8", maxBuffer: 1 << 28 });
  return ran.error ? { started: false, error: ran.error.code } : { started: true, status: ran.status };
};

const results = [];
for (const [name, build] of Object.entries(shapes)) {
  let low = 1;
  let high = name.includes("path") ? 40_000 : TIPS;
  if (starts(build(high)).started) {
    results.push({ shape: name, refused_at: null, started_up_to: high, argv_bytes_at_largest: bytes(build(high)) });
    continue;
  }
  while (high - low > 1) {
    const middle = Math.floor((low + high) / 2);
    if (starts(build(middle)).started) low = middle;
    else high = middle;
  }
  const refused = starts(build(high));
  results.push({
    shape: name,
    largest_started: low,
    smallest_refused: high,
    argv_bytes_started: bytes(build(low)),
    argv_bytes_refused: bytes(build(high)),
    pointer_bytes_refused: build(high).length * 8,
    refusal: refused.error,
  });
}

// The two ways round a long list that the strategies name.
const listFile = path.join(root, "paths");
fs.writeFileSync(listFile, ["a.txt", "b.txt"].map((p) => `:(literal)${p}`).join("\0") + "\0");
const fromFile = git(["diff", "--quiet", tip, base, `--pathspec-from-file=${listFile}`, "--pathspec-file-nul"]);
const viaStdin = git(["rev-list", "--count", "--stdin"], { input: `${tip}\n--not\n${tips.join("\n")}\n` });
const viaArgv = starts(shapes["unpublished_ahead: rev-list --count T --not <origin tip>… --"](TIPS));

process.stdout.write(
  `${JSON.stringify(
    {
      git: git(["--version"]).stdout.trim(),
      arg_max: Number(spawnSync("getconf", ["ARG_MAX"], { encoding: "utf8" }).stdout.trim()),
      environment_bytes: Object.entries(process.env).reduce((t, [k, v]) => t + k.length + v.length + 2, 0),
      path_bytes: path100(0).length,
      results,
      pathspec_from_file_on_diff: { status: fromFile.status, said: fromFile.stderr.split("\n")[0] },
      rev_list_stdin_with_every_tip: { status: viaStdin.status, count: viaStdin.stdout.trim(), argv_form: viaArgv },
    },
    null,
    1,
  )}\n`,
);
fs.rmSync(root, { recursive: true, force: true });
