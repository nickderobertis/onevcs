#!/usr/bin/env node
// Invalidate old evidence before compilation/test startup; failed producers leave it incomplete.
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const directory = fileURLToPath(new URL("../target/budget-records/", import.meta.url));
const run_id = randomBytes(32).toString("hex");
const manifest = state => writeFileSync(`${directory}/export-invocation.json`, JSON.stringify({ state, run_id }));
// Export evidence has its own invocation, independent of ordinary recovery.
const retract = () => {
  for (const name of ["export.json", "export-source-lfs.json"]) rmSync(`${directory}/${name}`, { force: true });
};
const failed = reason => {
  // The cause and next action come first: a cleanup that fails as well must not hide them.
  console.error(`export producer: ${reason}\nnext: run 'just export-journeys' to regenerate complete current-build records`);
  try {
    manifest("failed");
    retract();
  } catch (error) {
    console.error(`export producer: could not mark ${directory} failed (${error.message}); remove it before reading budgets`);
  }
};
try {
  mkdirSync(directory, { recursive: true });
  manifest("started");
  retract();
} catch (error) {
  failed(`could not reset the records under ${directory}: ${error.message}`);
  process.exit(1);
}
const [program, ...args] = process.argv.slice(2);
if (!program) {
  failed("producer command is missing");
  process.exitCode = 2;
} else {
  const result = spawnSync(program, args, { stdio: "inherit", env: { ...process.env, ONEVCS_EXPORT_INVOCATION: run_id } });
  if (result.error || result.status !== 0) {
    failed(result.error?.message ?? `${program} ${args.join(" ")} failed with ${result.signal ? `signal ${result.signal}` : `exit ${result.status}`}`);
  }
  process.exitCode = result.status ?? 1;
}
