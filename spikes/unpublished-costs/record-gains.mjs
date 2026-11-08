#!/usr/bin/env node
// Stage, in a scratch copy of ONEVCS_HOME, the state a warm host read meets: every
// llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate,changed_behavior_has_e2e] it stages the record crates/onevcs/src/spike.rs on this same branch writes, refusing any version but that one; what it staged is printed and reported with each gain figure; this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree.
// branch's decision recorded, one session's branches moved, and six identities' bases
// gained 3, 8, 25, 40, 300 and 600 commits since their decisions were recorded — the
// 25-, 300- and 600-commit gains landing 1, 3 and 2 recorded branches, the others none.
//
//   node spikes/unpublished-costs/record-gains.mjs --home COPY --session ID --listing FILE
//
// The record is the `decision-record` prototype's, written by a cold read over COPY.
// A base cannot be moved on a real checkout, so the gain is staged the other way round:
// each unlanded decision of the identity is re-dated to the commit N first-parent steps
// below the base it was decided against, which is exactly the state a record written N
// commits ago is in. A "landing" in the gain is a branch whose real landing commit lies
// in those N commits: its decision is re-dated and set back to `no`, as it was before it
// landed. A moved branch is one whose decision is removed, as a moved tip misses its key.
// LISTING is the engine's `unpublished --host --format json` output over COPY, which
// names each row's session. Prints what it staged, per identity, by label only.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const usage = "record-gains: --home COPY --session ID --listing FILE are required, and nothing else";
const given = {};
for (let at = 2; at < process.argv.length; at += 2) {
  const [name, value] = [process.argv[at], process.argv[at + 1]];
  if (!["--home", "--session", "--listing"].includes(name) || value === undefined || value.startsWith("--")) {
    process.stderr.write(`${usage}; ${JSON.stringify(name)} is not one of them or has no value\n`);
    process.exit(2);
  }
  given[name] = value;
}
const { "--home": home, "--session": session, "--listing": listingFile } = given;
if (!home || !session || !listingFile) {
  process.stderr.write(`${usage}\n`);
  process.exit(2);
}
const refuse = (message) => {
  process.stderr.write(`record-gains: ${message}\n`);
  process.exit(1);
};
const label = (kind, name) => `${kind}-${createHash("sha256").update(name).digest("hex").slice(0, 8)}`;
const dir = path.join(home, "cache", "spike-decisions");
let listing = null;
try {
  listing = JSON.parse(fs.readFileSync(listingFile, "utf8"));
} catch (error) {
  refuse(`--listing ${listingFile} cannot be read as JSON: ${error.message}`);
}
if (!Array.isArray(listing?.rows)) refuse(`--listing ${listingFile} is not \`unpublished --host --format json\` output: it holds no rows`);
const moved = new Set(listing.rows.filter((row) => row.manager_session === session).map((row) => `${row.identity}\t${row.branch}`));
const identityOf = new Map(listing.rows.map((row) => [row.branch, row.identity]));
const git = (repo, args) => {
  const ran = spawnSync("git", ["-C", repo, ...args], { encoding: "utf8", maxBuffer: 1 << 28 });
  if (ran.error || ran.status !== 0) refuse(`git ${args.join(" ")} in ${repo} failed: ${ran.error?.message || ran.stderr.trim()}`);
  return ran.stdout.trim();
};

const records = fs
  .readdirSync(dir)
  .filter((name) => name.endsWith(".json"))
  .map((name) => {
    const file = path.join(dir, name);
    let document = null;
    try {
      document = JSON.parse(fs.readFileSync(file, "utf8"));
    } catch (error) {
      refuse(`${file} cannot be read as JSON: ${error.message}`);
    }
    // The decision-record prototype's document, at the version spike.rs writes.
    if (document?.version !== 1 || typeof document.entries !== "object" || document.entries === null)
      refuse(`${file} is not a version-1 decision record; write it with a cold read under --prototype decision-record`);
    return { file, document };
  });

// A moved branch misses its key, which is the state removing its decision leaves.
let removed = 0;
for (const record of records) {
  for (const [key, entry] of Object.entries(record.document.entries)) {
    const identity = identityOf.get(entry.branch);
    if (identity && moved.has(`${identity}\t${entry.branch}`)) {
      delete record.document.entries[key];
      removed += 1;
    }
  }
}

/** For one record: its base commit, the first-parent line below it, and where each landed decision's evidence sits on it. */
function survey(record) {
  const entries = Object.entries(record.document.entries);
  const unlanded = entries.filter(([, entry]) => !["yes"].includes(entry.verdict.state) && entry.base_commit);
  if (!unlanded.length) return null;
  const [, sample] = unlanded[0];
  const line = git(sample.repo, ["rev-list", "--first-parent", "-n", "700", sample.base_commit]).split("\n").filter(Boolean);
  const position = new Map(line.map((sha, at) => [sha, at]));
  const landed = entries
    .filter(([, entry]) => entry.verdict.state === "yes" && entry.repo === sample.repo && entry.base === sample.base)
    .map(([key, entry]) => ({ key, entry, at: position.get(entry.verdict.evidence?.commit) }))
    .filter((found) => found.at !== undefined);
  return { record, line, landed, base: sample.base_commit };
}

const surveyed = records.map(survey).filter(Boolean);
const plan = [
  { gain: 3, land: 0 },
  { gain: 8, land: 0 },
  { gain: 25, land: 1 },
  { gain: 40, land: 0 },
  { gain: 300, land: 3 },
  { gain: 600, land: 2 },
];
const used = new Set();
const staged = [];
// Hardest first: the gains that must land branches take the identities that can.
for (const step of [...plan].sort((a, b) => b.land - a.land || b.gain - a.gain)) {
  const fits = (candidate) =>
    !used.has(candidate) &&
    candidate.line.length > step.gain &&
    candidate.landed.filter((found) => found.at < step.gain).length >= step.land &&
    (step.land > 0 || candidate.landed.every((found) => found.at >= step.gain));
  const chosen = surveyed.find(fits);
  if (!chosen) {
    staged.push({ ...step, identity: null, why: "no identity of this host has that history and those landings" });
    continue;
  }
  used.add(chosen);
  const from = chosen.line[step.gain];
  const landing = chosen.landed.filter((found) => found.at < step.gain).slice(0, step.land);
  let redated = 0;
  for (const entry of Object.values(chosen.record.document.entries)) {
    if (entry.verdict.state !== "yes" && entry.base_commit === chosen.base) {
      entry.base_commit = from;
      redated += 1;
    }
  }
  for (const found of landing) {
    found.entry.verdict = { state: "no" };
    found.entry.base_commit = from;
  }
  staged.push({
    ...step,
    record: path.basename(chosen.record.file, ".json").slice(0, 8),
    redated_unlanded: redated,
    landing: landing.map((found) => label("branch", found.entry.branch)),
  });
}
for (const record of records) fs.writeFileSync(record.file, JSON.stringify(record.document));
process.stdout.write(`${JSON.stringify({ removed_for_moved_session: removed, moved_rows: moved.size, staged }, null, 1)}\n`);
