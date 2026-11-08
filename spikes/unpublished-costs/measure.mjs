#!/usr/bin/env node
// One measurement of one `unpublished` read, printed as one JSON document.
// llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate,changed_behavior_has_e2e] it reads the trace crates/onevcs/src/spike.rs on this same branch writes, and harness.test.mjs drives each measure kind it was asked to; the attribution arithmetic is checked by the report figures it produced; this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree.
//
//   node spikes/unpublished-costs/measure.mjs --measure <MEASURE>
//        [--onevcs-home DIR] [--binary PATH] [--prototype NAME[,NAME]]
//        [--session ID] [--branch NAME --repo IDENTITY] [--runs-root DIR]
//        [--acknowledgements DIR] [--onevcs PATH] [--no-trace] [--count]
//        [--rows-out FILE] [--baseline-rows FILE] [--fixture DIR]
//        [--stage-record DIR]
//
// MEASURE is host, host-warm, session, stop-guard, acknowledge, recoverable-host,
// record-write or fixture-build. Each measure is one timed, untraced run of the read
// (wall, user and system time, load before and after, rows), then — unless
// --no-trace — one traced run of the same read whose spans and git reads attribute
// the time to causes. --count adds a third run under `strace -f` counting every
// program the read starts and every network connection it opens; it is never timed.
//
// --stage-record DIR copies a `decision-record` prototype record (as record-gains.mjs
// stages one) into --onevcs-home's cache before every run, so each run of a warm read
// meets the same staged state rather than the one the run before it wrote. It needs
// --onevcs-home, so it can only ever touch a scratch copy.
//
// The harness reads this host's state and records nothing into it. It points
// `--acknowledge` at a fresh scratch directory (a copy of --acknowledgements where one is
// named), and the reads keep their derived caches under ONEVCS_HOME as they always do.
// Only two options write under ONEVCS_HOME themselves, and both refuse unless
// --onevcs-home names a directory other than this user's real one: --stage-record
// replaces the decision record there, and --measure record-write removes it before each
// run, so that run is the one writing it.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const MEASURES = [
  "host",
  "host-warm",
  "session",
  "stop-guard",
  "acknowledge",
  "recoverable-host",
  "record-write",
  "fixture-build",
];

// The prototypes `crates/onevcs/src/spike.rs` switches on, by the names it reads.
const PROTOTYPES = ["local-head", "no-paths", "native-unbounded", "native-off", "decision-record", "batch-cat-file", "stdin-lists"];

function fail(message, code = 2) {
  process.stderr.write(`measure: ${message}\n`);
  process.exit(code);
}

/** A path as the filesystem names it, so a symlink cannot alias a guarded directory. */
function canonical(name) {
  try {
    return fs.realpathSync(path.resolve(name));
  } catch {
    return path.resolve(name);
  }
}

function parse(argv) {
  const options = { trace: true, count: false };
  const valued = new Set([
    "--measure",
    "--onevcs-home",
    "--binary",
    "--prototype",
    "--session",
    "--branch",
    "--repo",
    "--runs-root",
    "--acknowledgements",
    "--onevcs",
    "--rows-out",
    "--baseline-rows",
    "--fixture",
    "--scale",
    "--stage-record",
  ]);
  for (let at = 0; at < argv.length; at += 1) {
    const flag = argv[at];
    if (flag === "--no-trace") options.trace = false;
    else if (flag === "--count") options.count = true;
    else if (valued.has(flag)) {
      const value = argv[at + 1];
      if (value === undefined || value.startsWith("--")) fail(`${flag} needs a value`);
      options[flag.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = value;
      at += 1;
    } else fail(`unknown argument ${JSON.stringify(flag)}; see the header of ${process.argv[1]}`);
  }
  if (!options.measure) fail(`--measure is required: one of ${MEASURES.join(", ")}`);
  // This user's real home, which the reads default to; a journey's own scratch home may
  // be what ONEVCS_HOME names, so that is not the test.
  const real = path.join(os.homedir(), ".onevcs");
  for (const [asked, flag] of [[options.stageRecord, "--stage-record"], [options.measure === "record-write", "--measure record-write"]]) {
    if (!asked) continue;
    if (!options.onevcsHome) fail(`${flag} needs --onevcs-home naming a scratch copy of ONEVCS_HOME`);
    // llmlint: ignore[boundary_inputs_validated] this harness rewrites only a scratch copy named by --onevcs-home, which the journeys point ONEVCS_HOME at as well, so the real home cannot be told by that variable; ~/.onevcs is the directory it guards, and this measurement code lives only on the preserved spike branch, which the plan's run discards.
    if (canonical(options.onevcsHome) === canonical(real))
      fail(`${flag} writes the decision record under --onevcs-home, which names this user's real ONEVCS_HOME; name a scratch copy`);
  }
  if (options.stageRecord && canonical(options.stageRecord).startsWith(path.join(canonical(options.onevcsHome), "cache")))
    fail("--stage-record must name a directory outside the scratch home's cache, which each run replaces");
  const unknown = (options.prototype || "").split(",").filter((name) => name && !PROTOTYPES.includes(name));
  if (unknown.length) fail(`--prototype ${unknown.join(",")} is no prototype; the prototypes are ${PROTOTYPES.join(", ")}`);
  if (!MEASURES.includes(options.measure))
    fail(`unknown measure ${JSON.stringify(options.measure)}: one of ${MEASURES.join(", ")}`);
  return options;
}

const options = parse(process.argv.slice(2));
const scratchRoot = process.env.ONEPIPELINE_NODE_SCRATCH_DIR || os.tmpdir();
const scratch = fs.mkdtempSync(path.join(scratchRoot, "measure-"));
const binary = options.binary || "onepipeline";
const onevcs = options.onevcs || "onevcs";

/** A stable, non-identifying label for an identity or branch name. */
function label(kind, name) {
  if (!name) return "";
  return `${kind}-${createHash("sha256").update(name).digest("hex").slice(0, 8)}`;
}

function load() {
  return os.loadavg().map((value) => Math.round(value * 100) / 100);
}

/** The environment one run starts with. */
function environment({ trace, prototype }) {
  const env = { ...process.env };
  delete env.ONEVCS_SPIKE_TRACE;
  delete env.ONEVCS_SPIKE_PROTOTYPE;
  if (options.onevcsHome) env.ONEVCS_HOME = path.resolve(options.onevcsHome);
  if (options.runsRoot) env.ONEPIPELINE_RUNS_DIR = path.resolve(options.runsRoot);
  if (trace) env.ONEVCS_SPIKE_TRACE = trace;
  if (prototype) env.ONEVCS_SPIKE_PROTOTYPE = prototype;
  return env;
}

/** Run argv once under /usr/bin/time, answering wall, user, sys, status and output. */
function timed(argv, { env, input, cwd }) {
  const times = path.join(scratch, `time-${process.hrtime.bigint()}.txt`);
  const before = load();
  const started = process.hrtime.bigint();
  const ran = spawnSync("/usr/bin/time", ["-f", "%e %U %S", "-o", times, ...argv], {
    env,
    input,
    cwd: cwd || scratch,
    maxBuffer: 1 << 30,
    encoding: "utf8",
  });
  const wall = Number(process.hrtime.bigint() - started) / 1e9;
  if (ran.error) fail(`could not start ${argv[0]}: ${ran.error.message}`, 1);
  const [, user, sys] = fs.existsSync(times)
    ? fs.readFileSync(times, "utf8").trim().split(/\s+/).slice(-3).map(Number)
    : [null, null, null];
  return {
    wall_seconds: Math.round(wall * 1000) / 1000,
    user_seconds: user,
    sys_seconds: sys,
    load_before: before,
    load_after: load(),
    status: ran.status,
    stdout: ran.stdout,
    stderr: ran.stderr,
  };
}

/** The read this measure makes, as argv, stdin and working directory. */
function read(acknowledgements) {
  const ack = ["--acknowledgements", acknowledgements];
  switch (options.measure) {
    case "host":
    case "host-warm":
    case "record-write":
      return { argv: [binary, "unpublished", "--host", "--format", "json", ...ack] };
    case "session":
      if (!options.session) fail("--measure session needs --session ID");
      return {
        argv: [binary, "unpublished", "--session", options.session, "--format", "json", ...ack],
      };
    case "stop-guard": {
      if (!options.session) fail("--measure stop-guard needs --session ID");
      const input = JSON.stringify({
        session_id: options.session,
        transcript_path: path.join(scratch, "transcript.jsonl"),
        cwd: scratch,
        hook_event_name: "Stop",
        stop_hook_active: false,
      });
      return {
        argv: [
          binary,
          "stop-guard",
          "--format",
          "claude-code",
          "--unpublished",
          "--unpublished-acknowledgements",
          acknowledgements,
        ],
        input,
      };
    }
    case "acknowledge": {
      if (!options.session || !options.branch)
        fail("--measure acknowledge needs --session ID and --branch NAME (and --repo where several name it)");
      const repo = options.repo ? ["--repo", options.repo] : [];
      return {
        argv: [
          binary,
          "unpublished",
          "--session",
          options.session,
          "--acknowledge",
          options.branch,
          ...repo,
          "--reason",
          "spike-unpublished-costs measurement into a scratch directory",
          ...ack,
        ],
      };
    }
    case "recoverable-host":
      return { argv: [onevcs, "recoverable", "--detail", "decision", "--json"] };
    default:
      return fail(`no read for ${options.measure}`);
  }
}

/** The acknowledgements directory one run reads: the named one, or a fresh scratch one. */
function acknowledgementsFor(run) {
  if (options.acknowledgements && options.measure !== "acknowledge")
    return path.resolve(options.acknowledgements);
  const fresh = path.join(scratch, `acknowledged-${run}`);
  fs.mkdirSync(fresh, { recursive: true });
  // An acknowledgement is recorded into a copy, never into the directory it names.
  if (options.acknowledgements && fs.existsSync(options.acknowledgements))
    fs.cpSync(options.acknowledgements, fresh, { recursive: true });
  return fresh;
}

/** Each row reduced to the fields a decision is compared by. */
function rows(stdout) {
  let document;
  try {
    document = JSON.parse(stdout);
  } catch {
    return null;
  }
  // llmlint: ignore[boundary_inputs_validated] this document is the engine's own `--format json` output for the read this harness just spawned, not external input, and a schema mismatch shows as a measurement difference; this measurement code lives only on the preserved spike branch, which the plan's run discards.
  const listing = document?.listing || document;
  if (!Array.isArray(listing?.rows)) return null;
  if (listing.rows.some((row) => typeof row?.identity !== "string" || typeof row?.branch !== "string"))
    fail("a listing row carries no identity or branch string, so this is not the listing a decision is read from", 1);
  return {
    verdict: listing.verdict,
    unresolved: (listing.unresolved || []).length,
    rows: listing.rows
      .map((row) => {
        const word = row.landed?.state ?? null;
        return {
          identity: label("identity", row.identity),
          branch: label("branch", row.branch),
          tip: row.tip ?? null,
          landed: word,
          owed: Boolean(row.counted),
          in_flight: Boolean(row.in_flight),
          acknowledged: row.acknowledgement !== null && row.acknowledgement !== undefined,
          unknown: word === "unknown",
          launcher: row.manager_session ?? null,
        };
      })
      .sort((a, b) => (a.identity + a.branch + a.tip).localeCompare(b.identity + b.branch + b.tip)),
  };
}

function summarizeRows(decided) {
  if (!decided) return null;
  const count = (field) => decided.rows.filter((row) => row[field]).length;
  return {
    verdict: decided.verdict,
    rows: decided.rows.length,
    identities: new Set(decided.rows.map((row) => row.identity)).size,
    owed: count("owed"),
    in_flight: count("in_flight"),
    acknowledged: count("acknowledged"),
    unknown: count("unknown"),
    unresolved: decided.unresolved,
    launchers: Object.entries(
      decided.rows.reduce((counts, row) => {
        if (row.launcher) counts[row.launcher] = (counts[row.launcher] || 0) + 1;
        return counts;
      }, {}),
    )
      .sort((a, b) => b[1] - a[1])
      .slice(0, 5),
  };
}

/**
 * Two row sets compared by what a decision is: every row missing or added, each of a
 * row's tip, landed, owed, in-flight, acknowledged and unknown fields that differs, and
 * the verdict. A row's launcher and the listing's unresolved count are reported beside
 * the rows but are not decisions, so they are not compared.
 */
function compare(baseline, candidate) {
  const key = (row) => `${row.identity}/${row.branch}`;
  const by = new Map(baseline.rows.map((row) => [key(row), row]));
  const differences = [];
  for (const row of candidate.rows) {
    const was = by.get(key(row));
    if (!was) {
      differences.push({ row: key(row), change: "added" });
      continue;
    }
    by.delete(key(row));
    for (const field of ["tip", "landed", "owed", "in_flight", "acknowledged", "unknown"]) {
      if (was[field] !== row[field]) differences.push({ row: key(row), field, was: was[field], now: row[field] });
    }
  }
  for (const missing of by.keys()) differences.push({ row: missing, change: "missing" });
  if (baseline.verdict !== candidate.verdict)
    differences.push({ field: "verdict", was: baseline.verdict, now: candidate.verdict });
  return { identical: differences.length === 0, differences };
}

function readTrace(file) {
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => {
      // llmlint: ignore[boundary_inputs_validated] the trace is written by the spike instrumentation on this same branch for the run this harness just spawned, and its required fields are checked below; this measurement code lives only on the preserved spike branch, which the plan's run discards.
      const event = JSON.parse(line);
      if (typeof event?.kind !== "string" || typeof event.name !== "string" || typeof event.t !== "number" || typeof event.ms !== "number")
        fail(`${file} holds a trace event without kind, name, t and ms: ${line.slice(0, 200)}`, 1);
      // The engine's spans carry no extra payload; a git event's is what the arithmetic reads.
      if (event.kind === "git" && (typeof event.extra?.how !== "string" || typeof event.extra.argv_bytes !== "number"))
        fail(`${file} holds a git event without how and argv_bytes: ${line.slice(0, 200)}`, 1);
      return event;
    });
}

const round = (ms) => Math.round(ms) / 1000;

/**
 * Attribute a traced run's wall time to causes.
 *
 * Serial spans are taken as they stand. The identity scans run on a pool of threads,
 * so their wall time is attributed along the critical thread: the worker whose last
 * identity ended last, whose spans, summed, are what the scans waited on. Inside a
 * span, the git reads it made are split out by how they were answered (spawned git,
 * in process, stored proof, memo), since a thread makes them one after another.
 */
function attribute(events, wall) {
  const engine = events.filter((event) => event.kind === "engine");
  const spans = events.filter((event) => event.kind === "span");
  const gits = events.filter((event) => event.kind === "git");
  const start = Math.min(...events.map((event) => event.t));
  const end = Math.max(...events.map((event) => event.t + event.ms));
  const causes = [];
  const cause = (name, ms, method, owner) => {
    if (ms > 0.5) causes.push({ cause: name, seconds: round(ms), method, owner });
  };
  const sum = (list) => list.reduce((total, event) => total + event.ms, 0);
  const named = (list, name) => list.filter((event) => event.name === name);
  const within = (outer, list) =>
    list.filter((event) => event.t >= outer.t - 0.01 && event.t + event.ms <= outer.t + outer.ms + 0.01);

  const processMs = wall * 1000;
  const traced = events.length ? end - start : 0;
  cause(
    "process start, argument parsing and exit outside every span",
    processMs - traced,
    "wall minus the traced interval",
    "onepipeline (binary start), the OS",
  );
  const reads = [...named(engine, "recovery-read"), ...named(engine, "host-recovery-read")];
  for (const name of ["registration-lock", "every-landing", "acknowledgements-and-rows", "render"]) {
    cause(`engine: ${name}`, sum(named(engine, name)), "engine span", "onepipeline (unpublished.rs)");
  }
  let identityScans = 0;
  const perRead = [];
  for (const outer of reads) {
    const inner = within(outer, spans);
    let attributed = 0;
    for (const name of ["registry-and-rules", "session-records", "event-streams"]) {
      const ms = sum(named(inner, name));
      attributed += ms;
      cause(`onevcs: ${name} (${outer.name})`, ms, "onevcs span", "onevcs (vcs::collected)");
    }
    const scans = named(inner, "identity-scans");
    for (const scan of scans) {
      attributed += scan.ms;
      identityScans += scan.ms;
      const identities = named(within(scan, spans), "identity");
      const byThread = new Map();
      for (const identity of identities) {
        const list = byThread.get(identity.thread) || [];
        list.push(identity);
        byThread.set(identity.thread, list);
      }
      let critical = null;
      let latest = -Infinity;
      for (const [thread, list] of byThread) {
        const last = Math.max(...list.map((event) => event.t + event.ms));
        if (last > latest) {
          latest = last;
          critical = thread;
        }
      }
      const onCritical = spans.filter((event) => event.thread === critical && event.t >= scan.t && event.t <= scan.t + scan.ms);
      const gitsOnCritical = gits.filter((event) => event.thread === critical && event.t >= scan.t && event.t <= scan.t + scan.ms);
      const criticalIdentities = named(onCritical, "identity");
      let criticalMs = 0;

      const waits = onCritical.filter((event) => event.name.endsWith("-turn-wait"));
      const waitsIn = (span) =>
        sum(waits.filter((wait) => wait !== span && wait.t >= span.t && wait.t + wait.ms <= span.t + span.ms + 0.01));
      for (const name of ["identity-turn-wait", "checkout-turn-wait"]) {
        cause(
          `onevcs scan, critical thread: waiting for the ${name === "identity-turn-wait" ? "identity's" : "checkout's"} shared read turn`,
          sum(named(onCritical, name)),
          "turn-wait spans on the critical thread",
          "onevcs (checkout_reads), held by another process's exclusive turn",
        );
      }
      criticalMs += sum(waits);
      for (const name of [
        "resolve-and-base",
        "census",
        "checkout-open",
        "branch-listing",
        "landing-decision",
        "retirement-class",
        "row-holders-and-command",
      ]) {
        const list = named(onCritical, name);
        const ms = sum(list) - list.reduce((total, span) => total + waitsIn(span), 0);
        criticalMs += ms;
        const gitIn = gitsOnCritical.filter((event) => list.some((span) => event.t >= span.t && event.t <= span.t + span.ms));
        const spawned = sum(gitIn.filter((event) => event.extra.how === "spawn"));
        const native = sum(gitIn.filter((event) => event.extra.how === "native"));
        const proof = sum(gitIn.filter((event) => event.extra.how === "proof"));
        cause(
          `onevcs scan, critical thread: ${name} — spawned git`,
          spawned,
          "git events inside the span on the critical thread",
          "onevcs (git::run_unless), git",
        );
        cause(
          `onevcs scan, critical thread: ${name} — in-process answers`,
          native,
          "git events inside the span on the critical thread",
          "onevcs (native_objects, libgit2)",
        );
        cause(
          `onevcs scan, critical thread: ${name} — stored proofs`,
          proof,
          "git events inside the span on the critical thread",
          "onevcs (recovery_cache)",
        );
        cause(
          `onevcs scan, critical thread: ${name} — in process apart from git reads`,
          ms - spawned - native - proof,
          "span minus the git events inside it",
          "onevcs",
        );
      }
      const identityMs = sum(criticalIdentities);
      cause(
        "onevcs scan, critical thread: identity scan apart from its named spans",
        identityMs - criticalMs,
        "identity spans minus their named spans",
        "onevcs (vcs::scanned)",
      );
      cause(
        "onevcs scan: the critical thread waiting between identities",
        scan.ms - identityMs,
        "scan span minus the critical thread's identity spans",
        "onevcs (vcs::concurrently)",
      );
      perRead.push({
        read: outer.name,
        seconds: round(outer.ms),
        identities: identities.length,
        threads: byThread.size,
        critical_thread_identities: criticalIdentities.map((event) => ({
          identity: label("identity", event.identity),
          seconds: round(event.ms),
        })),
      });
    }
    cause(
      `onevcs: recovery read apart from its spans (${outer.name})`,
      outer.ms - attributed,
      "engine span minus onevcs spans",
      "onevcs (vcs::collected: narrowing, joining, sorting)",
    );
  }
  const engineTop = ["recovery-read", "host-recovery-read", "registration-lock", "every-landing", "acknowledgements-and-rows", "render"];
  const engineOuter = engine.filter((event) => !engineTop.includes(event.name));
  for (const outer of engineOuter) {
    // a span holding others (the stop guard's, or the listing after an acknowledgement)
    const inner = engine.filter((event) => event !== outer && event.t >= outer.t && event.t + event.ms <= outer.t + outer.ms + 0.01 && engineTop.includes(event.name));
    cause(`engine: ${outer.name} apart from the reads inside it`, outer.ms - sum(inner), "engine span minus engine spans inside it", "onepipeline");
  }
  const topEngine = engine.filter(
    (event) => !engine.some((other) => other !== event && other.t <= event.t && other.t + other.ms >= event.t + event.ms && other.ms > event.ms),
  );
  cause(
    "engine: between and around its spans",
    traced - sum(topEngine),
    "traced interval minus the outermost engine spans",
    "onepipeline (verbs, argument and file reads, output)",
  );
  const attributedSeconds = causes.reduce((total, entry) => total + entry.seconds, 0);
  for (const entry of causes) entry.share = Math.round((entry.seconds / wall) * 1000) / 10;
  causes.sort((a, b) => b.seconds - a.seconds);

  // All threads, not only the critical one: what the read spent, summed.
  const byShape = new Map();
  for (const event of gits) {
    const key = `${event.extra.how}\t${event.name}`;
    const entry = byShape.get(key) || { how: event.extra.how, shape: event.name, count: 0, seconds: 0, max_argv_bytes: 0, failed: 0 };
    entry.count += 1;
    entry.seconds += event.ms / 1000;
    entry.max_argv_bytes = Math.max(entry.max_argv_bytes, event.extra.argv_bytes);
    if (typeof event.extra.status === "string") entry.failed += 1;
    byShape.set(key, entry);
  }
  const shapes = [...byShape.values()]
    .map((entry) => ({ ...entry, seconds: Math.round(entry.seconds * 1000) / 1000 }))
    .sort((a, b) => b.seconds - a.seconds);
  const byIdentity = new Map();
  for (const event of spans.filter((span) => span.name === "identity")) {
    byIdentity.set(event.identity, { identity: label("identity", event.identity), seconds: round(event.ms), spawned: 0, spawn_seconds: 0, branches: new Set() });
  }
  for (const event of gits) {
    const entry = byIdentity.get(event.identity);
    if (!entry) continue;
    if (event.extra.how === "spawn") {
      entry.spawned += 1;
      entry.spawn_seconds += event.ms / 1000;
    }
  }
  const byBranch = new Map();
  for (const event of spans.filter((span) => ["landing-decision", "retirement-class", "row-holders-and-command"].includes(span.name))) {
    const key = `${event.identity}\t${event.branch}`;
    const entry = byBranch.get(key) || { identity: label("identity", event.identity), branch: label("branch", event.branch), seconds: 0, decision: 0, classify: 0, row: 0, spawned: 0 };
    entry.seconds += event.ms / 1000;
    entry[{ "landing-decision": "decision", "retirement-class": "classify", "row-holders-and-command": "row" }[event.name]] += event.ms / 1000;
    byBranch.set(key, entry);
    byIdentity.get(event.identity)?.branches.add(event.branch);
  }
  for (const event of gits.filter((git) => git.extra.how === "spawn")) {
    const entry = byBranch.get(`${event.identity}\t${event.branch}`);
    if (entry) entry.spawned += 1;
  }
  const fix = (value) => Math.round(value * 1000) / 1000;
  const refused = gits
    .filter((event) => typeof event.extra.status === "string")
    .map((event) => ({ shape: event.name, identity: label("identity", event.identity), branch: label("branch", event.branch), argc: event.extra.argc, argv_bytes: event.extra.argv_bytes, error: event.extra.status }));
  return {
    traced_wall_seconds: wall,
    attributed_seconds: Math.round(attributedSeconds * 1000) / 1000,
    attributed_share: Math.round((attributedSeconds / wall) * 1000) / 10,
    causes,
    scans: perRead,
    git: {
      spawned: gits.filter((event) => event.extra.how === "spawn").length,
      native: gits.filter((event) => event.extra.how === "native").length,
      batch: gits.filter((event) => event.extra.how === "batch").length,
      proof: gits.filter((event) => event.extra.how === "proof").length,
      memo: gits.filter((event) => event.extra.how === "memo").length,
      thread_seconds_by_how: Object.fromEntries(
        ["spawn", "native", "proof", "memo", "batch"].map((how) => [how, fix(sum(gits.filter((event) => event.extra.how === how)) / 1000)]),
      ),
      by_shape: shapes.slice(0, 40),
      refused,
    },
    span_thread_seconds: Object.fromEntries(
      [...new Set(spans.map((span) => span.name))].map((name) => [name, fix(sum(named(spans, name)) / 1000)]),
    ),
    identities: [...byIdentity.values()]
      .map((entry) => ({ ...entry, branches: entry.branches.size, spawn_seconds: fix(entry.spawn_seconds) }))
      .sort((a, b) => b.seconds - a.seconds),
    slowest_branches: [...byBranch.values()]
      .map((entry) => ({ ...entry, seconds: fix(entry.seconds), decision: fix(entry.decision), classify: fix(entry.classify), row: fix(entry.row) }))
      .sort((a, b) => b.seconds - a.seconds)
      .slice(0, 15),
  };
}

/** Every program the read starts and every connection it opens, under strace. */
function counted(argv, env, input) {
  const log = path.join(scratch, "strace.log");
  const ran = spawnSync("strace", ["-f", "-qq", "-s", "256", "-e", "trace=execve,connect", "-o", log, ...argv], {
    env,
    input,
    cwd: scratch,
    maxBuffer: 1 << 30,
    encoding: "utf8",
  });
  if (ran.error) return { error: ran.error.message };
  const programs = {};
  const connections = { inet: 0, inet6: 0, unix: 0 };
  let listedOnStdin = 0;
  for (const line of fs.readFileSync(log, "utf8").split("\n")) {
    const exec = line.match(/execve\("([^"]+)"/);
    if (exec && !line.includes("ENOENT")) {
      const name = path.basename(exec[1]);
      programs[name] = (programs[name] || 0) + 1;
      if (name === "git" && line.includes('"--stdin"')) listedOnStdin += 1;
    }
    if (line.includes("connect(")) {
      if (line.includes("AF_INET6")) connections.inet6 += 1;
      else if (line.includes("AF_INET")) connections.inet += 1;
      else if (line.includes("AF_UNIX")) connections.unix += 1;
    }
  }
  return { status: ran.status, programs, connections, git_with_stdin_list: listedOnStdin };
}

/** Replace the scratch home's decision record with the staged one. */
function stageRecord() {
  if (!options.stageRecord) return;
  const into = path.join(path.resolve(options.onevcsHome), "cache", "spike-decisions");
  if (!fs.statSync(options.stageRecord, { throwIfNoEntry: false })?.isDirectory())
    fail(`--stage-record ${options.stageRecord} is no directory holding a decision record; stage one with record-gains.mjs`);
  fs.rmSync(into, { recursive: true, force: true });
  fs.cpSync(path.resolve(options.stageRecord), into, { recursive: true });
}

/** Under --measure record-write, remove the scratch home's record so the run writes it. */
function clearRecord() {
  if (options.measure !== "record-write") return;
  fs.rmSync(path.join(path.resolve(options.onevcsHome), "cache", "spike-decisions"), { recursive: true, force: true });
}

function runRead(run, prototype, trace) {
  stageRecord();
  clearRecord();
  const acknowledgements = acknowledgementsFor(run);
  const { argv, input, cwd } = read(acknowledgements);
  const env = environment({ trace, prototype });
  return { ...timed(argv, { env, input, cwd }), argv, acknowledgements, env, input };
}

function strip(result) {
  const { stdout, stderr, env, input, ...rest } = result;
  return { ...rest, stderr_tail: (stderr || "").split("\n").slice(-6).join("\n") };
}

function measureRead() {
  const prototype = options.prototype || "";
  const document = {
    measure: options.measure,
    onevcs_home: options.onevcsHome ? path.resolve(options.onevcsHome) : process.env.ONEVCS_HOME || path.join(os.homedir(), ".onevcs"),
    runs_root: options.runsRoot || process.env.ONEPIPELINE_RUNS_DIR || null,
    binary,
    prototype: prototype || null,
    staged_record: options.stageRecord ? path.resolve(options.stageRecord) : null,
    cores: os.cpus().length,
  };
  if (options.measure === "host-warm") {
    // A first read fills whatever a later read reuses — the proofs, and with the
    // decision-record prototype, the record — and is reported but not the figure.
    const priming = runRead("priming", prototype, null);
    document.priming = { ...strip(priming), decided: summarizeRows(rows(priming.stdout)) };
  }
  const timedRun = runRead("timed", prototype, null);
  const decided = rows(timedRun.stdout);
  document.timed = { ...strip(timedRun), decided: summarizeRows(decided) };
  if (options.rowsOut && decided) fs.writeFileSync(options.rowsOut, JSON.stringify(decided, null, 1));
  if (options.baselineRows && decided) {
    let baseline = null;
    try {
      baseline = JSON.parse(fs.readFileSync(options.baselineRows, "utf8"));
    } catch (error) {
      fail(`--baseline-rows ${options.baselineRows} cannot be read as JSON: ${error.message}`, 1);
    }
    // llmlint: ignore[boundary_inputs_validated] the baseline is a --rows-out document this harness wrote itself for the same comparison; this measurement code lives only on the preserved spike branch, which the plan's run discards.
    const decisions = ["owed", "in_flight", "acknowledged", "unknown"];
    const nullableText = (value) => value === null || typeof value === "string";
    const malformed = (row) => typeof row?.identity !== "string" || typeof row?.branch !== "string" || !nullableText(row.tip) || !nullableText(row.landed) || decisions.some((field) => typeof row[field] !== "boolean");
    const rowsOf = Array.isArray(baseline?.rows) ? baseline.rows : null;
    const keys = new Set((rowsOf || []).map((row) => `${row?.identity}/${row?.branch}`));
    if (!rowsOf || rowsOf.some(malformed) || keys.size !== rowsOf.length)
      fail(`--baseline-rows ${options.baselineRows} is not a --rows-out document: it needs rows, each once, with an identity, a branch and boolean decision fields`, 1);
    document.rows_against_baseline = compare(baseline, decided);
  }
  if (options.trace) {
    const trace = path.join(scratch, "trace.ndjson");
    const traced = runRead("traced", prototype, trace);
    const events = readTrace(trace);
    document.traced = { ...strip(traced), decided: summarizeRows(rows(traced.stdout)) };
    document.attribution = attribute(events, traced.wall_seconds);
    if (options.measure === "record-write") {
      const writes = events.filter((event) => event.kind === "record");
      document.record = summarizeRecord(writes);
    } else if (events.some((event) => event.kind === "record")) {
      document.record = summarizeRecord(events.filter((event) => event.kind === "record"));
    }
  }
  if (options.count) {
    const ack = acknowledgementsFor("counted");
    const { argv, input } = read(ack);
    stageRecord();
    clearRecord();
    document.counted = counted(argv, environment({ prototype }), input);
  }
  // Spawned git by the trace; a batched read also keeps one process per checkout, which
  // only --count's strace sees, so with a batch there is no count without it.
  document.git_processes = document.counted?.programs?.git ?? (document.attribution?.git.batch > 0 ? null : (document.attribution?.git.spawned ?? null));
  return document;
}

function summarizeRecord(writes) {
  const groups = {};
  for (const event of writes) {
    const entry = groups[event.name] || { count: 0, seconds: 0, max_ms: 0, extra: [] };
    entry.count += 1;
    entry.seconds += event.ms / 1000;
    entry.max_ms = Math.max(entry.max_ms, event.ms);
    if (event.extra && Object.keys(event.extra).length && entry.extra.length < 12) entry.extra.push(event.extra);
    groups[event.name] = entry;
  }
  for (const entry of Object.values(groups)) {
    entry.mean_ms = Math.round((entry.seconds * 1000 * 1000) / entry.count) / 1000;
    entry.seconds = Math.round(entry.seconds * 1000) / 1000;
    entry.max_ms = Math.round(entry.max_ms * 1000) / 1000;
  }
  return groups;
}

function measureFixtureBuild() {
  if (!options.binary) fail("--measure fixture-build needs --binary naming the fixture builder (spikes/unpublished-costs/fixture)");
  const fixture = options.fixture ? path.resolve(options.fixture) : path.join(scratch, "fixture");
  const argv = [options.binary, "--scale", options.scale || "one", "--out", fixture];
  const ran = timed(argv, { env: environment({}) });
  let built = null;
  try {
    built = JSON.parse(ran.stdout);
  } catch {
    built = null;
  }
  // A build that failed, or printed no account of what it built, is no measurement.
  const usable = ran.status === 0 && typeof built?.home === "string" && typeof built?.launcher === "string";
  return {
    measure: "fixture-build",
    scale: options.scale || "one",
    fixture,
    ...strip(ran),
    built,
    usable,
  };
}

const document = options.measure === "fixture-build" ? measureFixtureBuild() : measureRead();
process.stdout.write(`${JSON.stringify(document, null, 1)}\n`);
// The read's own answers — 0 none, 7 owed, 1 unanswered — are measurements; a refusal
// (2), a program that could not start (126, 127) or a signal is a measurement not made.
// A fixture build has no such answers: it built what it says, or it is no measurement.
if (document.measure === "fixture-build") process.exit(document.usable ? 0 : 1);
const status = document.timed?.status;
process.exit([0, 1, 7].includes(status) ? 0 : 1);
