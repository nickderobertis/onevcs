// llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate] it writes the record shape crates/onevcs/src/spike.rs on this same branch reads; this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree.
// The harness's journey: drive `measure.mjs` the way the report's figures were taken,
// over a generated host — a real origin, a registered checkout and one unpublished
// branch — with the real engine and onevcs, and its two failure paths.
//
//   SPIKE_ENGINE=<onepipeline built by build.sh> [SPIKE_ONEVCS=<onevcs>] \
//     node --test spikes/unpublished-costs/harness.test.mjs
//
// It refuses rather than skips without SPIKE_ENGINE: a journey that passed without the
// engine it measures would prove nothing.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const measure = path.join(here, "measure.mjs");
const engine = process.env.SPIKE_ENGINE;
const onevcs = process.env.SPIKE_ONEVCS || "onevcs";
assert.ok(engine && fs.existsSync(engine), "SPIKE_ENGINE must name the engine build.sh built");

function host() {
  const root = fs.mkdtempSync(path.join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR || os.tmpdir(), "harness-"));
  const home = path.join(root, "home");
  const origin = path.join(root, "origin.git");
  const checkout = path.join(root, "checkout");
  fs.mkdirSync(home);
  const env = {
    ...process.env,
    ONEVCS_HOME: home,
    HOME: root,
    XDG_STATE_HOME: path.join(root, "state"),
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_AUTHOR_NAME: "Harness",
    GIT_AUTHOR_EMAIL: "harness@example.invalid",
    GIT_COMMITTER_NAME: "Harness",
    GIT_COMMITTER_EMAIL: "harness@example.invalid",
  };
  const run = (argv, cwd = root) => {
    const ran = spawnSync(argv[0], argv.slice(1), { cwd, env, encoding: "utf8" });
    assert.equal(ran.status, 0, `${argv.join(" ")}: ${ran.stderr}`);
    return ran.stdout;
  };
  run(["git", "init", "-q", "--bare", "-b", "main", origin]);
  run(["git", "init", "-q", "-b", "main", checkout]);
  fs.writeFileSync(path.join(checkout, "a.txt"), "a\n");
  run(["git", "add", "a.txt"], checkout);
  run(["git", "commit", "-q", "-m", "chore: seed"], checkout);
  run(["git", "remote", "add", "origin", origin], checkout);
  run(["git", "push", "-q", "origin", "main"], checkout);
  run(["git", "fetch", "-q", "origin"], checkout);
  run(["git", "remote", "set-head", "origin", "main"], checkout);
  run(["git", "checkout", "-q", "-b", "work/left-behind"], checkout);
  fs.writeFileSync(path.join(checkout, "b.txt"), "b\n");
  run(["git", "add", "b.txt"], checkout);
  run(["git", "commit", "-q", "-m", "feat: work nobody published"], checkout);
  run(["git", "checkout", "-q", "main"], checkout);
  run([onevcs, "register", checkout]);
  return { root, home, env };
}

function measureWith(args, env) {
  const ran = spawnSync("node", [measure, ...args], { env, encoding: "utf8", maxBuffer: 1 << 28 });
  return { status: ran.status, stderr: ran.stderr, document: ran.status === 0 ? JSON.parse(ran.stdout) : null };
}

test("the host read is timed, attributed and its owed branch counted", () => {
  const { root, home, env } = host();
  const { status, stderr, document } = measureWith(
    ["--measure", "host", "--binary", engine, "--onevcs-home", home, "--rows-out", path.join(root, "rows.json")],
    env,
  );
  assert.equal(status, 0, stderr);
  assert.equal(document.timed.status, 7, "one unpublished branch is owed");
  assert.equal(document.timed.decided.owed, 1);
  assert.ok(document.timed.wall_seconds > 0);
  assert.equal(document.timed.load_before.length, 3);
  assert.ok(document.attribution.causes.length > 0, "the traced run attributes causes");
  assert.ok(document.attribution.attributed_share >= 80, `attributed ${document.attribution.attributed_share}%`);
  assert.ok(document.attribution.causes.some((cause) => cause.cause.startsWith("onevcs scan")));
  assert.equal(typeof document.git_processes, "number");
  const rows = JSON.parse(fs.readFileSync(path.join(root, "rows.json"), "utf8"));
  assert.equal(rows.rows.length, 1);
  assert.ok(!JSON.stringify(rows).includes("left-behind"), "rows carry labels, never branch names");
});

test("a prototype's rows are compared field by field with a baseline", () => {
  const { root, home, env } = host();
  const baseline = path.join(root, "baseline.json");
  measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--rows-out", baseline], env);
  const same = measureWith(
    ["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--prototype", "no-paths,local-head", "--baseline-rows", baseline],
    env,
  );
  assert.equal(same.status, 0, same.stderr);
  assert.deepEqual(same.document.rows_against_baseline, { identical: true, differences: [] });
  const altered = JSON.parse(fs.readFileSync(baseline, "utf8"));
  altered.rows[0].owed = false;
  fs.writeFileSync(baseline, JSON.stringify(altered));
  const differs = measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--baseline-rows", baseline], env);
  assert.equal(differs.document.rows_against_baseline.identical, false);
  assert.deepEqual(
    differs.document.rows_against_baseline.differences.map(({ field, was, now }) => ({ field, was, now })),
    [{ field: "owed", was: false, now: true }],
  );
});

test("the batch and stdin prototypes answer as the unmodified read does, and the batch is what answered", () => {
  const { root, home, env } = host();
  const baseline = path.join(root, "baseline.json");
  measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--prototype", "native-off", "--rows-out", baseline], env);
  const ran = measureWith(
    ["--measure", "host", "--binary", engine, "--onevcs-home", home, "--prototype", "native-off,batch-cat-file,stdin-lists", "--baseline-rows", baseline],
    env,
  );
  assert.equal(ran.status, 0, ran.stderr);
  assert.deepEqual(ran.document.rows_against_baseline, { identical: true, differences: [] });
  assert.ok(ran.document.attribution.git.batch > 0, JSON.stringify(ran.document.attribution.git));
});

test("the decision record is written on a cold read and reused on a warm one", () => {
  const { home, env } = host();
  const { status, stderr, document } = measureWith(
    ["--measure", "host-warm", "--binary", engine, "--onevcs-home", home, "--prototype", "decision-record"],
    env,
  );
  assert.equal(status, 0, stderr);
  assert.equal(document.priming.decided.owed, 1);
  assert.equal(document.timed.decided.owed, 1, "a reused decision answers what the derivation did");
  assert.ok(fs.readdirSync(path.join(home, "cache", "spike-decisions")).length === 1);
  assert.equal(document.record["write-entry"], undefined, "a warm read with nothing moved writes nothing");
  assert.ok(document.record.key.count >= 1);
});

test("a measure missing what it reads is refused naming it, and an unknown one is refused", () => {
  const { env } = host();
  const missing = measureWith(["--measure", "acknowledge", "--binary", engine, "--session", "s"], env);
  assert.equal(missing.status, 2);
  assert.match(missing.stderr, /--branch NAME/);
  const unknown = measureWith(["--measure", "everything", "--binary", engine], env);
  assert.equal(unknown.status, 2);
  assert.match(unknown.stderr, /unknown measure "everything"/);
});

test("a read that cannot start is a measurement not made", () => {
  const { home, env } = host();
  const ran = measureWith(["--measure", "host", "--binary", path.join(home, "no-such-engine"), "--onevcs-home", home, "--no-trace"], env);
  assert.equal(ran.status, 1);
});

test("a staged decision record is what every run of the read meets, and needs a scratch home", () => {
  const { root, home, env } = host();
  const cold = measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--prototype", "decision-record"], env);
  assert.equal(cold.document.timed.decided.owed, 1);
  // Stage the record as record-gains.mjs does: the one decision rewritten, here to a
  // landing, so a read that met the staged record withholds the branch it was owed.
  const staged = path.join(root, "staged");
  fs.cpSync(path.join(home, "cache", "spike-decisions"), staged, { recursive: true });
  for (const name of fs.readdirSync(staged)) {
    const file = path.join(staged, name);
    const record = JSON.parse(fs.readFileSync(file, "utf8"));
    for (const entry of Object.values(record.entries))
      entry.verdict = { state: "yes", evidence: { tier: "recorded-landing", commit: entry.tip } };
    fs.writeFileSync(file, JSON.stringify(record));
  }
  const args = ["--measure", "host", "--binary", engine, "--prototype", "decision-record", "--stage-record", staged];
  const warm = measureWith([...args, "--onevcs-home", home], env);
  assert.equal(warm.status, 0, warm.stderr);
  assert.equal(warm.document.timed.decided.owed, 0, "the timed run met the staged landing");
  assert.equal(warm.document.traced.decided.owed, 0, "so did the traced run after it");
  assert.equal(warm.document.staged_record, staged);
  const unscoped = measureWith(args, env);
  assert.equal(unscoped.status, 2);
  assert.match(unscoped.stderr, /--stage-record needs --onevcs-home/);
});

test("a comparison names a row missing, a row added and a verdict that moved", () => {
  const { root, home, env } = host();
  const baseline = path.join(root, "baseline.json");
  measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--rows-out", baseline], env);
  const altered = JSON.parse(fs.readFileSync(baseline, "utf8"));
  const [row] = altered.rows;
  altered.rows = [{ ...row, branch: "branch-elsewhere" }];
  altered.verdict = "none";
  fs.writeFileSync(baseline, JSON.stringify(altered));
  const ran = measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--baseline-rows", baseline], env);
  assert.equal(ran.status, 0, ran.stderr);
  assert.deepEqual(
    ran.document.rows_against_baseline.differences.map(({ change, field, was, now }) => ({ change, field, was, now })),
    [
      { change: "added", field: undefined, was: undefined, now: undefined },
      { change: "missing", field: undefined, was: undefined, now: undefined },
      { change: undefined, field: "verdict", was: "none", now: "owed" },
    ],
  );
  fs.writeFileSync(baseline, JSON.stringify({ rows: [{}] }));
  const malformed = measureWith(["--measure", "host", "--binary", engine, "--onevcs-home", home, "--no-trace", "--baseline-rows", baseline], env);
  assert.equal(malformed.status, 1);
  assert.match(malformed.stderr, /is not a --rows-out document/);
});

test("the session read and the stop guard are timed over a launcher's session", () => {
  const { home, env } = host();
  for (const measure of ["session", "stop-guard"]) {
    const ran = measureWith(["--measure", measure, "--binary", engine, "--onevcs-home", home, "--session", "no-launcher-here"], env);
    assert.equal(ran.status, 0, `${measure}: ${ran.stderr}`);
    assert.ok(ran.document.timed.wall_seconds > 0);
    assert.ok(ran.document.attribution.attributed_share >= 80, `${measure} attributed ${ran.document.attribution.attributed_share}%`);
  }
});

test("a fixture build that fails, or says nothing of what it built, is no measurement", () => {
  const { root, env } = host();
  const builder = path.join(root, "builder");
  fs.writeFileSync(builder, "#!/bin/sh\necho 'not json'\nexit 0\n", { mode: 0o755 });
  const silent = measureWith(["--measure", "fixture-build", "--binary", builder, "--fixture", path.join(root, "f")], env);
  assert.equal(silent.status, 1);
  fs.writeFileSync(builder, "#!/bin/sh\necho 'unpublished-costs-fixture: cannot run git' >&2\nexit 1\n", { mode: 0o755 });
  const failed = spawnSync("node", [measure, "--measure", "fixture-build", "--binary", builder, "--fixture", path.join(root, "g")], { env, encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.equal(JSON.parse(failed.stdout).usable, false);
  assert.match(JSON.parse(failed.stdout).stderr_tail, /cannot run git/);
});

test("a read's spawned cat-file -e calls replay batched per checkout with the same answers", () => {
  const { root, home, env } = host();
  const ack = path.join(root, "ack");
  fs.mkdirSync(ack);
  const replay = (binary) =>
    spawnSync("node", [path.join(here, "batch-replay.mjs"), "--binary", binary, "--acknowledgements", ack, "--onevcs-home", home], { env, encoding: "utf8" });
  const ran = replay(engine);
  assert.equal(ran.status, 0, ran.stderr);
  const document = JSON.parse(ran.stdout);
  assert.equal(document.read_status, 7);
  assert.ok(document.calls > 0 && document.checkouts >= 1, JSON.stringify(document));
  assert.equal(document.answers_agree, true);
  const broken = replay(path.join(root, "no-such-engine"));
  assert.equal(broken.status, 1);
  assert.match(broken.stderr, /the counting read did not run to an answer/);
});
